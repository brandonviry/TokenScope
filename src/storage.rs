use crate::model::{Config, UsageEvent};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

static EVENT_WRITE: Mutex<()> = Mutex::new(());

#[derive(Clone)]
pub struct Store {
    pub root: PathBuf,
}

impl Store {
    pub fn new(root: Option<PathBuf>) -> Self {
        let root = root.unwrap_or_else(|| {
            directories::ProjectDirs::from("fr", "TokenScope", "TokenScope")
                .map(|d| d.data_local_dir().to_path_buf())
                .unwrap_or_else(|| PathBuf::from("tokenscope-data"))
        });
        Self { root }
    }
    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.json")
    }
    pub fn events_path(&self) -> PathBuf {
        self.root.join("events.jsonl")
    }
    pub fn ollama_path(&self) -> PathBuf {
        self.root.join("ollama.jsonl")
    }
    pub fn load_config(&self) -> (Config, Option<String>) {
        let path = self.config_path();
        if !path.exists() {
            return (Config::default(), None);
        }
        match fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|b| serde_json::from_slice::<Config>(&b).map_err(|e| e.to_string()))
        {
            Ok(mut config) => {
                config.refresh_seconds = config.refresh_seconds.clamp(15, 3600);
                config.alert_percent = config.alert_percent.clamp(1.0, 100.0);
                if config.schema_version < 3 {
                    if config.schema_version < 2
                        && !config.providers.iter().any(|p| p.kind == crate::model::ProviderKind::Antigravity)
                        && let Some(provider) = Config::default().providers.into_iter().find(|p| p.kind == crate::model::ProviderKind::Antigravity)
                    {
                        let index = config.providers.iter().position(|p| p.kind == crate::model::ProviderKind::OllamaCloud).unwrap_or(config.providers.len());
                        config.providers.insert(index, provider);
                    }
                    if !config.providers.iter().any(|p| p.kind == crate::model::ProviderKind::Qoder)
                        && let Some(mut provider) = Config::default().providers.into_iter().find(|p| p.kind == crate::model::ProviderKind::Qoder)
                    {
                        if config.providers.iter().any(|p| p.id == provider.id) {
                            provider.id = uuid::Uuid::new_v4().to_string();
                        }
                        config.providers.push(provider);
                    }
                    config.schema_version = 3;
                    if let Err(error) = self.save_config(&config) {
                        return (config, Some(format!("Mise à jour des fournisseurs en mémoire. Échec de l’enregistrement : {error}")));
                    }
                }
                (config, None)
            }
            Err(e) => (
                Config::default(),
                Some(format!(
                    "Configuration illisible : {e}. Le fichier d’origine est conservé. Vérifiez-le avant d’enregistrer."
                )),
            ),
        }
    }
    pub fn save_config(&self, config: &Config) -> Result<(), String> {
        if config.providers.iter().any(|p| p.name.trim().is_empty()) {
            return Err("Chaque fournisseur doit avoir un nom.".into());
        }
        let mut ids = HashSet::new();
        for p in &config.providers {
            if p.id.is_empty() || !ids.insert(&p.id) {
                return Err("Les identifiants des fournisseurs doivent être uniques.".into());
            }
            for limit in &p.limits {
                if !limit.amount.is_finite() || limit.amount <= 0.0 {
                    return Err("Une limite doit être strictement positive.".into());
                }
                if limit.manual_used.is_some_and(|v| !v.is_finite() || v < 0.0)
                    || (limit.manual_used.is_some() && limit.manual_reset.is_none())
                {
                    return Err("Un relevé manuel doit avoir une consommation positive et une date de remise à zéro.".into());
                }
            }
        }
        if self.config_path().exists() {
            // Keep the last saved configuration, including an unreadable original.
            fs::copy(self.config_path(), self.root.join("config.backup.json"))
                .map_err(|e| e.to_string())?;
        }
        self.write_json("config.json", config)
    }
    pub fn write_json<T: serde::Serialize>(&self, name: &str, value: &T) -> Result<(), String> {
        fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        let temporary = self.root.join(format!("{name}.tmp"));
        let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
        let mut file = fs::File::create(&temporary).map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&temporary, self.root.join(name)).map_err(|e| e.to_string())
    }
    pub fn append_event(&self, event: &UsageEvent, ollama: bool) -> Result<(), String> {
        if !event.valid() {
            return Err(
                "Consommation invalide : valeurs négatives ou cache supérieur aux entrées.".into(),
            );
        }
        let _guard = EVENT_WRITE.lock().map_err(|e| e.to_string())?;
        fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        let path = if ollama {
            self.ollama_path()
        } else {
            self.events_path()
        };
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        let mut bytes = serde_json::to_vec(event).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        file.write_all(&bytes)
            .and_then(|_| file.flush())
            .map_err(|e| e.to_string())
    }
    pub fn import(&self, path: &Path, provider: &str) -> Result<usize, String> {
        let mut events = vec![];
        if path
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("csv"))
        {
            let mut reader = csv::Reader::from_path(path).map_err(|e| e.to_string())?;
            for (index, row) in reader.deserialize::<UsageEvent>().enumerate() {
                let mut event = row.map_err(|e| format!("Ligne CSV {} : {e}", index + 2))?;
                event.provider = provider.into();
                event.source = "Import CSV".into();
                if !event.valid() {
                    return Err(format!("Ligne CSV {} invalide", index + 2));
                }
                events.push(event);
            }
        } else {
            let reader = BufReader::new(fs::File::open(path).map_err(|e| e.to_string())?);
            for (index, line) in reader.lines().enumerate() {
                let line = line.map_err(|e| e.to_string())?;
                if line.trim().is_empty() {
                    continue;
                }
                let mut event: UsageEvent = serde_json::from_str(&line)
                    .map_err(|e| format!("Ligne JSONL {} : {e}", index + 1))?;
                event.provider = provider.into();
                event.source = "Import JSONL".into();
                if !event.valid() {
                    return Err(format!("Ligne JSONL {} invalide", index + 1));
                }
                events.push(event);
            }
        }
        // Validate the whole import before appending anything. Reimports keep their IDs.
        let mut ids = HashSet::new();
        if let Ok(file) = fs::File::open(self.events_path()) {
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if let Ok(e) = serde_json::from_str::<UsageEvent>(&line) {
                    ids.insert((e.provider, e.id));
                }
            }
        }
        let mut count = 0;
        for event in events {
            if ids.insert((event.provider.clone(), event.id.clone())) {
                self.append_event(&event, false)?;
                count += 1;
            }
        }
        Ok(count)
    }
}

pub fn export_csv(path: &Path, events: &[&UsageEvent]) -> Result<(), String> {
    let mut writer = csv::Writer::from_path(path).map_err(|e| e.to_string())?;
    // Explicit header also makes an empty export usable.
    writer
        .write_record([
            "id",
            "provider",
            "timestamp",
            "model",
            "input_tokens",
            "output_tokens",
            "cached_tokens",
            "cache_write_tokens",
            "cost_usd",
            "credits",
            "source",
            "tokens_reported",
            "request_count",
        ])
        .map_err(|e| e.to_string())?;
    for e in events {
        writer
            .write_record([
                spreadsheet_safe(&e.id),
                spreadsheet_safe(&e.provider),
                e.timestamp.to_rfc3339(),
                spreadsheet_safe(&e.model),
                e.input_tokens.to_string(),
                e.output_tokens.to_string(),
                e.cached_tokens.to_string(),
                e.cache_write_tokens.to_string(),
                e.cost_usd.map(|v| v.to_string()).unwrap_or_default(),
                e.credits.map(|v| v.to_string()).unwrap_or_default(),
                spreadsheet_safe(&e.source),
                e.tokens_reported.to_string(),
                e.request_count.to_string(),
            ])
            .map_err(|e| e.to_string())?;
    }
    writer.flush().map_err(|e| e.to_string())
}
fn spreadsheet_safe(value: &str) -> String {
    if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    #[test]
    fn upgrade_adds_antigravity_once_and_preserves_existing_settings() {
        let root =
            std::env::temp_dir().join(format!("tokenscope-upgrade-{}", uuid::Uuid::new_v4()));
        let store = Store::new(Some(root.clone()));
        fs::create_dir_all(&root).unwrap();
        let legacy = serde_json::json!({"providers":[{"id":"custom-id","name":"Mon compte","kind":"Jsonl","log_path":"D:/mes-donnees","limits":[{"name":"Mon budget","amount":12345.0}]}],"refresh_seconds":95,"alert_percent":73.0});
        fs::write(store.config_path(), serde_json::to_vec(&legacy).unwrap()).unwrap();
        let (mut config, warning) = store.load_config();
        assert!(warning.is_none());
        assert_eq!(config.schema_version, 3);
        assert_eq!(config.providers[0].id, "custom-id");
        assert_eq!(config.providers[0].log_path, "D:/mes-donnees");
        assert_eq!(config.providers[0].limits[0].amount, 12345.0);
        assert_eq!(config.refresh_seconds, 95);
        assert_eq!(config.alert_percent, 73.0);
        assert_eq!(
            config
                .providers
                .iter()
                .filter(|p| p.kind == crate::model::ProviderKind::Antigravity)
                .count(),
            1
        );
        let backup: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("config.backup.json")).unwrap()).unwrap();
        assert_eq!(backup, legacy);
        assert_eq!(config.providers.iter().filter(|p| p.kind == crate::model::ProviderKind::Qoder).count(), 1);
        config.providers.retain(|p| p.kind != crate::model::ProviderKind::Qoder);
        config
            .providers
            .retain(|p| p.kind != crate::model::ProviderKind::Antigravity);
        store.save_config(&config).unwrap();
        assert!(
            store
                .load_config()
                .0
                .providers
                .iter()
                .all(|p| p.kind != crate::model::ProviderKind::Antigravity)
        );
        assert!(store.load_config().0.providers.iter().all(|p| p.kind != crate::model::ProviderKind::Qoder));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn qoder_migration_preserves_existing_provider_and_id_collisions() {
        let root = std::env::temp_dir().join(format!("tokenscope-qoder-upgrade-{}", uuid::Uuid::new_v4()));
        let store = Store::new(Some(root.clone()));
        fs::create_dir_all(&root).unwrap();
        let mut config = Config { schema_version: 2, ..Config::default() };
        let qoder = config.providers.iter_mut().find(|p| p.kind == crate::model::ProviderKind::Qoder).unwrap();
        qoder.enabled = false;
        fs::write(store.config_path(), serde_json::to_vec(&config).unwrap()).unwrap();
        let loaded = store.load_config().0;
        assert_eq!(loaded.providers.iter().filter(|p| p.kind == crate::model::ProviderKind::Qoder).count(), 1);
        assert!(!loaded.providers.iter().find(|p| p.kind == crate::model::ProviderKind::Qoder).unwrap().enabled);
        config.providers.retain(|p| p.kind != crate::model::ProviderKind::Qoder);
        config.providers[0].id = "qoder".into();
        fs::write(store.config_path(), serde_json::to_vec(&config).unwrap()).unwrap();
        let loaded = store.load_config().0;
        assert_ne!(loaded.providers.iter().find(|p| p.kind == crate::model::ProviderKind::Qoder).unwrap().id, "qoder");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn import_is_idempotent_and_round_trips_csv() {
        let root = std::env::temp_dir().join(format!("tokenscope-{}", uuid::Uuid::new_v4()));
        let store = Store::new(Some(root.clone()));
        fs::create_dir_all(&root).unwrap();
        let event = UsageEvent {
            id: "request-1".into(),
            provider: "a".into(),
            timestamp: Utc::now(),
            model: "model".into(),
            input_tokens: 100,
            output_tokens: 20,
            cached_tokens: 60,
            cache_write_tokens: 0,
            cost_usd: Some(0.01),
            credits: None,
            source: "test".into(),
            tokens_reported: true,
            request_count: 1,
        };
        let path = root.join("export.csv");
        export_csv(&path, &[&event]).unwrap();
        assert_eq!(store.import(&path, "a").unwrap(), 1);
        assert_eq!(store.import(&path, "a").unwrap(), 0);
        store.save_config(&Config::default()).unwrap();
        store.save_config(&Config::default()).unwrap();
        assert!(store.load_config().1.is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
