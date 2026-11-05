//! Secrets never enter Config or application logs. Environment variables take
//! precedence, then the operating system's credential vault.
use crate::model::ProviderConfig;

pub fn environment(name: &str) -> Option<String> {
    if let Ok(value) = std::env::var(name)
        && !value.trim().is_empty()
    {
        return Some(value.trim().into());
    }
    #[cfg(windows)]
    {
        use winreg::{
            RegKey,
            enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ},
        };
        // A desktop app may have inherited an environment older than a terminal.
        for (hive, path) in [
            (HKEY_CURRENT_USER, "Environment"),
            (
                HKEY_LOCAL_MACHINE,
                r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
            ),
        ] {
            if let Ok(registry) = RegKey::predef(hive).open_subkey_with_flags(path, KEY_READ)
                && let Ok(value) = registry.get_value::<String, _>(name)
                && !value.trim().is_empty()
            {
                return Some(value.trim().into());
            }
        }
    }
    None
}

pub fn stored(id: &str) -> Option<String> {
    keyring::Entry::new("TokenScope", id)
        .ok()?
        .get_password()
        .ok()
        .filter(|key| !key.trim().is_empty())
}
pub fn read(provider: &ProviderConfig) -> Result<String, String> {
    environment(&provider.key_env)
        .or_else(|| stored(&provider.id))
        .or_else(|| {
            if provider.kind == crate::model::ProviderKind::OllamaCloud {
                ollama_existing().map(|(key, _)| key)
            } else {
                None
            }
        })
        .ok_or_else(|| {
            format!(
                "Clé non détectée ({}). Configurez ce compte dans Connexions.",
                provider.key_env
            )
        })
}
pub fn ollama_existing() -> Option<(String, String)> {
    let home = directories::UserDirs::new()?.home_dir().to_path_buf();
    for path in [
        home.join(".local/share/opencode/auth.json"),
        home.join(".config/opencode/auth.json"),
        home.join("AppData/Roaming/opencode/auth.json"),
    ] {
        let Some(value) = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        else {
            continue;
        };
        for name in ["ollama-cloud", "ollama"] {
            let entry = &value[name];
            if entry["type"] == "api"
                && let Some(key) = entry["key"].as_str().filter(|s| !s.trim().is_empty())
            {
                return Some((key.trim().into(), format!("OpenCode · {}", path.display())));
            }
        }
    }
    None
}
pub fn save(id: &str, key: &str) -> Result<(), String> {
    if key.trim().is_empty() {
        return Err("Saisissez une clé API.".into());
    }
    keyring::Entry::new("TokenScope", id)
        .and_then(|entry| entry.set_password(key.trim()))
        .map_err(|_| "Impossible d’enregistrer la clé dans le coffre du système.".into())
}
pub fn forget(id: &str) -> Result<(), String> {
    match keyring::Entry::new("TokenScope", id).and_then(|entry| entry.delete_credential()) {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err("Impossible d’effacer la clé du coffre du système.".into()),
    }
}
