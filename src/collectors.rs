use crate::{model::*, storage::Store};
use chrono::{DateTime, Duration, Utc};
use reqwest::blocking::Client;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Instant, SystemTime},
};

#[derive(Default, Clone, Copy)]
struct Counts {
    input: u64,
    output: u64,
    cached: u64,
    write: u64,
}
impl Counts {
    fn codex(v: &Value) -> Self {
        Self {
            input: n(v, "input_tokens"),
            output: n(v, "output_tokens"),
            cached: n(v, "cached_input_tokens"),
            write: n(v, "cache_write_input_tokens"),
        }
    }
    fn total(self) -> u64 {
        self.input.saturating_add(self.output)
    }
    fn delta(self, old: Self) -> Self {
        Self {
            input: self.input.saturating_sub(old.input),
            output: self.output.saturating_sub(old.output),
            cached: self.cached.saturating_sub(old.cached),
            write: self.write.saturating_sub(old.write),
        }
    }
}
#[derive(Default)]
struct FileState {
    kind: Option<ProviderKind>,
    offset: u64,
    modified: Option<SystemTime>,
    model: String,
    session: String,
    previous: Counts,
    events: HashMap<String, UsageEvent>,
    quota: Option<ProviderSnapshot>,
    invalid: usize,
}

pub struct Collector {
    store: Store,
    files: HashMap<(String, PathBuf), FileState>,
    live: HashMap<String, ProviderSnapshot>,
    last_network: HashMap<String, Instant>,
    client: Option<Client>,
    api_events: HashMap<String, Vec<UsageEvent>>,
}

impl Collector {
    pub fn new(store: Store) -> Self {
        let live = fs::read(store.root.join("quotas.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let api_events = fs::read(store.root.join("remote-events.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            store,
            files: HashMap::new(),
            live,
            last_network: HashMap::new(),
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(12))
                .connect_timeout(std::time::Duration::from_secs(6))
                .user_agent("TokenScope/1.0 (native usage monitor)")
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .ok(),
            api_events,
        }
    }
    pub fn refresh(&mut self, config: &Config, force: bool) -> Snapshot {
        let mut snapshot = Snapshot::default();
        let mut keep = HashSet::new();
        let mut fetched_groups = HashSet::new();
        let mut all_events: HashMap<(String, String), UsageEvent> = HashMap::new();
        for provider in config.providers.iter().filter(|p| p.enabled) {
            let mut local = ProviderSnapshot {
                id: provider.id.clone(),
                status: "Aucune consommation enregistrée".into(),
                ..Default::default()
            };
            let mut paths: Vec<PathBuf> = provider
                .log_path
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect();
            if provider.kind == ProviderKind::Codex {
                for path in paths.clone() {
                    if path.file_name().is_some_and(|s| s == "sessions") {
                        let archive = path.with_file_name("archived_sessions");
                        if archive.exists() {
                            paths.push(archive);
                        }
                    }
                }
            }
            for path in paths {
                if !path.exists() {
                    if path != self.store.ollama_path() {
                        snapshot.warnings.push(format!(
                            "{} : source introuvable : {}",
                            provider.name,
                            path.display()
                        ));
                    }
                    continue;
                }
                let files: Vec<PathBuf> = if path.is_file() {
                    vec![path]
                } else {
                    let mut files = vec![];
                    for entry in walkdir::WalkDir::new(&path).follow_links(false) {
                        match entry {
                            Ok(e)
                                if e.file_type().is_file()
                                    && e.path().extension().is_some_and(|s| s == "jsonl") =>
                            {
                                files.push(e.into_path())
                            }
                            Err(e) => snapshot
                                .warnings
                                .push(format!("{} : lecture partielle : {e}", provider.name)),
                            _ => {}
                        }
                    }
                    files
                };
                for file in files {
                    let key = (provider.id.clone(), file.clone());
                    keep.insert(key.clone());
                    let state = self.files.entry(key).or_default();
                    if let Err(e) = read_incremental(&file, state, provider) {
                        snapshot.warnings.push(format!(
                            "{} : {} : {e}",
                            provider.name,
                            file.display()
                        ));
                    }
                    if state.invalid > 0 {
                        snapshot.warnings.push(format!(
                            "{} : {} ligne(s) inutilisable(s) dans {}",
                            provider.name,
                            state.invalid,
                            file.file_name().unwrap_or_default().to_string_lossy()
                        ));
                    }
                    for event in state.events.values() {
                        merge_event(&mut all_events, event.clone());
                    }
                    if let Some(q) = &state.quota
                        && q.observed_at > local.observed_at
                    {
                        local = q.clone();
                    }
                    snapshot.files_scanned += 1;
                }
            }
            let count = all_events
                .values()
                .filter(|e| e.provider == provider.id)
                .count();
            if count > 0 && local.quotas.is_empty() {
                local.status = format!("{count} relevés locaux");
                local.source = "Journaux locaux".into();
            }
            if matches!(
                provider.kind,
                ProviderKind::Claude | ProviderKind::ClaudeCode
            ) {
                local.shared_group = Some(format!("claude:{}", provider.auth_path));
            }
            if provider.online {
                let group = if matches!(
                    provider.kind,
                    ProviderKind::Claude | ProviderKind::ClaudeCode
                ) {
                    format!("claude:{}", provider.auth_path)
                } else {
                    provider.id.clone()
                };
                let elapsed = self.last_network.get(&group).map(|t| t.elapsed().as_secs());
                let due = elapsed.is_none_or(|s| s >= if force { 20 } else { 180 });
                if due && fetched_groups.insert(group.clone()) {
                    self.last_network.insert(group.clone(), Instant::now());
                    let response = match provider.kind {
                        ProviderKind::Codex => self.fetch_codex(provider),
                        ProviderKind::Claude | ProviderKind::ClaudeCode => {
                            self.fetch_claude(provider)
                        }
                        ProviderKind::AnthropicApi => self.fetch_anthropic(provider),
                        ProviderKind::OllamaCloud => self.fetch_ollama_cloud(provider),
                        ProviderKind::Antigravity => self
                            .client
                            .as_ref()
                            .ok_or_else(|| "Connexion réseau non initialisée.".into())
                            .and_then(|client| crate::antigravity::fetch(client, provider)),
                        _ => Err("Pas de connexion distante pour ce type de source.".into()),
                    };
                    match response {
                        Ok(mut data) => {
                            data.live = true;
                            self.live.insert(group.clone(), data);
                        }
                        Err(e) => {
                            if let Some(old) = self.live.get_mut(&group) {
                                old.live = false;
                                old.status = e.clone();
                            } else {
                                self.live.insert(
                                    group.clone(),
                                    ProviderSnapshot {
                                        status: e,
                                        id: provider.id.clone(),
                                        ..Default::default()
                                    },
                                );
                            }
                        }
                    }
                }
                if let Some(online) = self.live.get(&group) {
                    if (!online.quotas.is_empty() || online.live)
                        && (online.live || online.observed_at >= local.observed_at)
                    {
                        let shared = local.shared_group.clone();
                        local = online.clone();
                        local.id = provider.id.clone();
                        local.shared_group = shared;
                    } else {
                        local.status = online.status.clone();
                        local.live = false;
                    }
                }
            } else if local.observed_at.is_some() {
                local.live = false;
                local.source = "Dernier relevé des journaux".into();
            }
            if let Some(events) = self.api_events.get(&provider.id) {
                for e in events {
                    merge_event(&mut all_events, e.clone());
                }
            }
            if provider.kind == ProviderKind::Ollama && count == 0 {
                local.status = "Collecteur Ollama ou import JSONL à configurer".into();
            }
            snapshot.providers.push(local);
        }
        self.files
            .retain(|key, _| key.0 == "manual" || keep.contains(key));
        for manual in [self.store.events_path(), self.store.ollama_path()] {
            if manual.exists() {
                let dummy = ProviderConfig {
                    id: "manual".into(),
                    ..Default::default()
                };
                let key = ("manual".into(), manual.clone());
                let state = self.files.entry(key).or_default();
                if let Err(e) = read_incremental(&manual, state, &dummy) {
                    snapshot.warnings.push(format!("Relevés enregistrés : {e}"));
                }
                if state.invalid > 0 {
                    snapshot.warnings.push(format!(
                        "Relevés enregistrés : {} ligne(s) invalide(s)",
                        state.invalid
                    ));
                }
                for e in state.events.values().filter(|e| {
                    config
                        .providers
                        .iter()
                        .any(|p| p.enabled && p.id == e.provider)
                }) {
                    merge_event(&mut all_events, e.clone());
                }
            }
        }
        snapshot.events = all_events.into_values().collect();
        snapshot.events.sort_unstable_by_key(|e| e.timestamp);
        snapshot.refreshed_at = Some(Utc::now());
        if let Err(e) = self.store.write_json("quotas.json", &self.live) {
            snapshot.warnings.push(format!("Cache des quotas : {e}"));
        }
        if let Err(e) = self
            .store
            .write_json("remote-events.json", &self.api_events)
        {
            snapshot
                .warnings
                .push(format!("Cache de l’historique distant : {e}"));
        }
        snapshot
    }
    fn get(&self, url: &str) -> Result<reqwest::blocking::RequestBuilder, String> {
        self.client
            .as_ref()
            .map(|c| c.get(url))
            .ok_or_else(|| "Impossible d’initialiser la connexion réseau.".into())
    }
    fn fetch_codex(&self, p: &ProviderConfig) -> Result<ProviderSnapshot, String> {
        let auth = read_auth(&p.auth_path)?;
        let token = auth
            .pointer("/tokens/access_token")
            .and_then(Value::as_str)
            .ok_or("Session Codex absente. Connectez-vous dans Codex.")?;
        let mut request = self
            .get("https://chatgpt.com/backend-api/wham/usage")?
            .bearer_auth(token);
        if let Some(id) = auth.pointer("/tokens/account_id").and_then(Value::as_str) {
            request = request.header("ChatGPT-Account-Id", id);
        }
        let value = read_response(request)?;
        let mut result = codex_quota(&value, p, Utc::now(), true);
        result.source = "Compte Codex · lecture seule".into();
        result.status = "Synchronisé avec le compte".into();
        if result.quotas.is_empty() {
            return Err("Le compte n’a pas renvoyé de limite exploitable.".into());
        }
        Ok(result)
    }
    fn fetch_claude(&self, p: &ProviderConfig) -> Result<ProviderSnapshot, String> {
        let auth = read_auth(&p.auth_path)?;
        let token = auth
            .pointer("/claudeAiOauth/accessToken")
            .and_then(Value::as_str)
            .ok_or("Session Claude absente. Connectez-vous dans Claude Code.")?;
        let value = read_response(
            self.get("https://api.anthropic.com/api/oauth/usage")?
                .bearer_auth(token)
                .header("anthropic-beta", "oauth-2025-04-20"),
        )?;
        let mut result = claude_quota(&value, p, Utc::now());
        result.plan = auth
            .pointer("/claudeAiOauth/subscriptionType")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if result.quotas.is_empty() {
            return Err("Claude n’a pas renvoyé de limite exploitable.".into());
        }
        Ok(result)
    }
    fn fetch_anthropic(&mut self, p: &ProviderConfig) -> Result<ProviderSnapshot, String> {
        let key = crate::keys::read(p)?;
        let start = (Utc::now() - Duration::days(90))
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .to_rfc3339();
        let mut events = vec![];
        for cost in [false, true] {
            let url = if cost {
                "https://api.anthropic.com/v1/organizations/analytics/cost"
            } else {
                "https://api.anthropic.com/v1/organizations/analytics/usage"
            };
            let mut next: Option<String> = None;
            for _ in 0..100 {
                let mut request = self
                    .get(url)?
                    .header("x-api-key", &key)
                    .header("anthropic-version", "2023-06-01")
                    .header("anthropic-beta", "analytics-usage-cost-2026-08-24")
                    .query(&[
                        ("starting_at", start.as_str()),
                        ("time_bucket_width", "1d"),
                        ("group_by[]", "model"),
                        ("limit", "1000"),
                    ]);
                if let Some(page) = &next {
                    request = request.query(&[("page", page)]);
                }
                let data = read_response(request)?;
                for row in data["data"].as_array().into_iter().flatten() {
                    let Some(timestamp) = parse_time(&row["starting_at"]) else {
                        continue;
                    };
                    let model = row
                        .pointer("/group/model")
                        .and_then(Value::as_str)
                        .unwrap_or("non attribué")
                        .to_string();
                    let u = &row["usage"];
                    let cached = n(u, "cache_read_input_tokens");
                    let write = n(&u["cache_creation"], "ephemeral_1h_input_tokens")
                        + n(&u["cache_creation"], "ephemeral_5m_input_tokens");
                    let input = n(u, "uncached_input_tokens") + cached + write;
                    let amount = row
                        .pointer("/cost/amount")
                        .and_then(number)
                        .map(|v| v / 100.0);
                    events.push(UsageEvent {
                        id: format!("api:{}:{timestamp}:{model}:{cost}", p.id),
                        provider: p.id.clone(),
                        timestamp,
                        model,
                        input_tokens: input,
                        output_tokens: n(u, "output_tokens"),
                        cached_tokens: cached,
                        cache_write_tokens: write,
                        cost_usd: amount,
                        credits: None,
                        source: "Anthropic Analytics · agrégat UTC journalier".into(),
                        tokens_reported: !cost,
                        request_count: if cost { 0 } else { n(u, "requests") },
                    });
                }
                if !data["has_more"].as_bool().unwrap_or(false) {
                    break;
                }
                next = data["next_page"].as_str().map(str::to_owned);
                if next.is_none() {
                    return Err("Pagination Anthropic incomplète : prochain curseur absent.".into());
                }
            }
        }
        self.api_events.insert(p.id.clone(), events);
        Ok(ProviderSnapshot {
            id: p.id.clone(),
            observed_at: Some(Utc::now()),
            source: "Anthropic Analytics API".into(),
            status: "90 jours synchronisés · agrégats UTC".into(),
            live: true,
            ..Default::default()
        })
    }
    fn fetch_ollama_cloud(&mut self, p: &ProviderConfig) -> Result<ProviderSnapshot, String> {
        let key = crate::keys::read(p)?;
        let balance = read_response(
            self.get("https://ollama.com/api/balance")?
                .bearer_auth(&key),
        );
        let usage = read_response(
            self.get("https://ollama.com/api/usage")?
                .bearer_auth(&key)
                .query(&[("range", "30d"), ("scope", "self"), ("group_by", "model")]),
        );
        let mut report = match &balance {
            Ok(value) => ollama_balance(value, p, Utc::now()),
            Err(_) => ProviderSnapshot {
                id: p.id.clone(),
                ..Default::default()
            },
        };
        match usage {
            Ok(value) => {
                if report.quotas.is_empty() {
                    report.quotas = ollama_legacy_usage(&value);
                }
                let new_events = ollama_events(&value, p);
                let missing_tokens = new_events.iter().any(|e| !e.tokens_reported);
                let requests = value
                    .pointer("/totals/request_count")
                    .and_then(Value::as_u64)
                    .unwrap_or_else(|| new_events.iter().map(|e| e.request_count).sum());
                let from =
                    parse_time(&value["from"]).unwrap_or_else(|| Utc::now() - Duration::days(31));
                let existing = self.api_events.entry(p.id.clone()).or_default();
                existing.retain(|event| event.timestamp < from);
                existing.extend(new_events);
                report.usage_note = Some(format!(
                    "{requests} requêtes / 30 j · {}",
                    if missing_tokens {
                        "tokens non fournis"
                    } else {
                        "données du compte"
                    }
                ));
                report.status = "Quotas et historique Ollama Cloud synchronisés".into();
                report.observed_at = Some(Utc::now());
                report.source = "Ollama Cloud API · lecture seule".into();
                report.live = true;
            }
            Err(e) if balance.is_ok() => {
                report.status = format!("Solde synchronisé. Historique : {e}");
                report.usage_note = Some("Dernier historique conservé.".into());
            }
            Err(e) => return Err(e),
        }
        if report.quotas.is_empty() && report.credit_balance.is_none() {
            return Err(balance
                .err()
                .unwrap_or_else(|| "Ollama n’a pas communiqué de solde ou de limite.".into()));
        }
        Ok(report)
    }
}

pub fn ollama_balance(value: &Value, p: &ProviderConfig, time: DateTime<Utc>) -> ProviderSnapshot {
    let mut report = ProviderSnapshot {
        id: p.id.clone(),
        observed_at: Some(time),
        source: "Ollama Cloud API · lecture seule".into(),
        status: "Solde Ollama Cloud synchronisé".into(),
        live: true,
        ..Default::default()
    };
    let included = &value["included"];
    if let (Some(balance), Some(allowance)) = (
        number(&included["balance_usd"]),
        number(&included["allowance_usd"]),
    ) {
        let used = (allowance - balance).max(0.0);
        report.quotas.push(Quota {
            name: "Crédits inclus · mois".into(),
            used_percent: if allowance > 0.0 {
                used / allowance * 100.0
            } else {
                0.0
            },
            resets_at: parse_time(&included["period"]["until"]),
            used: Some(used),
            limit: Some(allowance),
            unit: "USD".into(),
            ..Default::default()
        });
    } else {
        for (name, label, seconds) in [
            ("session", "Session · 5 h", 18000),
            ("weekly", "Hebdomadaire · 7 j", 604800),
        ] {
            if let Some(remaining) = number(&included[name]["remaining_percent"]) {
                report.quotas.push(Quota {
                    name: label.into(),
                    used_percent: 100.0 - remaining.clamp(0.0, 100.0),
                    resets_at: parse_time(&included[name]["resets_at"]),
                    window_seconds: Some(seconds),
                    unit: "%".into(),
                    ..Default::default()
                });
            }
        }
    }
    report.credit_balance =
        number(&value["purchased"]["balance_usd"]).map(|v| format!("{v:.2} USD"));
    report
}
fn ollama_legacy_usage(value: &Value) -> Vec<Quota> {
    [
        ("session", "Session · 5 h", 18000),
        ("weekly", "Hebdomadaire · 7 j", 604800),
    ]
    .into_iter()
    .filter_map(|(field, name, seconds)| {
        let window = &value["limits"][field];
        let usage = number(&window["usage"])?;
        Some(Quota {
            name: name.into(),
            used_percent: usage * 100.0,
            resets_at: parse_time(&window["resets_at"]).or_else(|| parse_time(&window["reset_at"])),
            window_seconds: Some(seconds),
            unit: "%".into(),
            ..Default::default()
        })
    })
    .collect()
}
pub fn ollama_events(value: &Value, p: &ProviderConfig) -> Vec<UsageEvent> {
    let mut events = vec![];
    for bucket in value["buckets"].as_array().into_iter().flatten() {
        let Some(timestamp) = parse_time(&bucket["from"]) else {
            continue;
        };
        let mut groups: Vec<(&Value, &str)> = bucket["groups"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|group| (group, group["model"].as_str().unwrap_or("inconnu")))
            .collect();
        if let Some(remainder) = bucket.get("remainder") {
            groups.push((remainder, "Autres modèles"));
        }
        if groups.is_empty() {
            groups.push((bucket, "Tous modèles"));
        }
        for (group, model) in groups {
            if n(group, "request_count") == 0
                && n(group, "input_tokens") == 0
                && number(&group["usage_usd"]).unwrap_or(0.0) == 0.0
            {
                continue;
            }
            let input = n(group, "input_tokens");
            events.push(UsageEvent {
                id: format!("ollama-cloud:{}:{timestamp}:{model}", p.id),
                provider: p.id.clone(),
                timestamp,
                model: model.into(),
                input_tokens: input,
                output_tokens: n(group, "output_tokens"),
                cached_tokens: n(group, "cached_input_tokens").min(input),
                cache_write_tokens: 0,
                cost_usd: number(&group["usage_usd"]),
                credits: None,
                source: "Ollama Cloud API · agrégat journalier UTC".into(),
                tokens_reported: group["input_tokens"].is_number()
                    && group["output_tokens"].is_number(),
                request_count: n(group, "request_count"),
            });
        }
    }
    events
}

fn read_auth(path: &str) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|_| {
        "Session locale introuvable ou illisible. Vérifiez le fichier de connexion.".to_string()
    })?;
    serde_json::from_slice(&bytes).map_err(|_| "Fichier de connexion invalide.".into())
}
fn read_response(request: reqwest::blocking::RequestBuilder) -> Result<Value, String> {
    let response = request.send().map_err(|_| {
        "Connexion impossible ou délai dépassé. Dernier relevé conservé.".to_string()
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            401 | 403 => {
                "Authentification expirée ou accès refusé. Vérifiez la session ou la clé dans Connexions."
                    .into()
            }
            429 => "Trop de requêtes. Nouvelle tentative automatique dans quelques minutes.".into(),
            code => format!("Service indisponible (HTTP {code}). Dernier relevé conservé."),
        });
    }
    response
        .json()
        .map_err(|_| "Réponse de consommation non reconnue.".into())
}

pub fn parse_time(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
        .or_else(|| v.as_i64().and_then(|n| DateTime::from_timestamp(n, 0)))
}
fn n(v: &Value, key: &str) -> u64 {
    v[key].as_u64().unwrap_or(0)
}
fn number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .filter(|n| n.is_finite() && *n >= 0.0)
}

pub fn codex_quota(
    value: &Value,
    p: &ProviderConfig,
    time: DateTime<Utc>,
    online: bool,
) -> ProviderSnapshot {
    let mut result = ProviderSnapshot {
        id: p.id.clone(),
        observed_at: Some(time),
        source: "Journal Codex".into(),
        status: "Dernier relevé local".into(),
        plan: value["plan_type"].as_str().map(str::to_owned),
        credit_balance: value.pointer("/credits/balance").and_then(|v| {
            v.as_str()
                .map(str::to_owned)
                .or_else(|| v.as_f64().map(|f| f.to_string()))
        }),
        unlimited_credits: value
            .pointer("/credits/unlimited")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        ..Default::default()
    };
    let root = if online { &value["rate_limit"] } else { value };
    let mut add = |root: &Value, prefix: &str| {
        for (field, base) in [
            (if online { "primary_window" } else { "primary" }, "Session"),
            (
                if online {
                    "secondary_window"
                } else {
                    "secondary"
                },
                "Hebdomadaire",
            ),
        ] {
            let window = &root[field];
            let Some(percent) = number(&window["used_percent"]) else {
                continue;
            };
            let seconds = window["limit_window_seconds"]
                .as_i64()
                .or_else(|| window["window_minutes"].as_i64().map(|m| m * 60));
            let label = match seconds {
                Some(18000) => "Session · 5 h".into(),
                Some(604800) => "Hebdomadaire · 7 j".into(),
                Some(s) => format!("{base} · {} h", s / 3600),
                None => base.into(),
            };
            result.quotas.push(Quota {
                name: format!("{prefix}{label}"),
                used_percent: percent,
                resets_at: parse_time(&window[if online { "reset_at" } else { "resets_at" }]),
                window_seconds: seconds,
                unit: "%".into(),
                ..Default::default()
            });
        }
    };
    add(root, "");
    if online {
        if !value["code_review_rate_limit"].is_null() {
            add(&value["code_review_rate_limit"], "Revue de code · ");
        }
        for extra in value["additional_rate_limits"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let label = extra["limit_name"]
                .as_str()
                .or_else(|| extra["limit_id"].as_str())
                .unwrap_or("Autre limite");
            add(
                extra.get("rate_limit").unwrap_or(extra),
                &format!("{label} · "),
            );
        }
    }
    result
}

pub fn claude_quota(value: &Value, p: &ProviderConfig, time: DateTime<Utc>) -> ProviderSnapshot {
    let mut result = ProviderSnapshot {
        id: p.id.clone(),
        observed_at: Some(time),
        source: "Compte Claude · quota partagé".into(),
        status: "Synchronisé avec le compte".into(),
        live: true,
        shared_group: Some(format!("claude:{}", p.auth_path)),
        ..Default::default()
    };
    let mut fields: Vec<_> = value
        .as_object()
        .into_iter()
        .flat_map(|m| m.iter())
        .filter(|(key, _)| key.as_str() == "five_hour" || key.starts_with("seven_day"))
        .collect();
    fields.sort_by_key(|(key, _)| match key.as_str() {
        "five_hour" => 0,
        "seven_day" => 1,
        _ => 2,
    });
    for (key, window) in fields {
        let Some(percent) = number(&window["utilization"]) else {
            continue;
        };
        let name = match key.as_str() {
            "five_hour" => "Session · 5 h".into(),
            "seven_day" => "Hebdomadaire · 7 j".into(),
            "seven_day_sonnet" => "Sonnet · 7 j".into(),
            "seven_day_opus" => "Opus · 7 j".into(),
            other => other.replace('_', " "),
        };
        result.quotas.push(Quota {
            name,
            used_percent: percent,
            resets_at: parse_time(&window["resets_at"]),
            window_seconds: Some(if key == "five_hour" { 18000 } else { 604800 }),
            used: number(&window["used_dollars"]),
            limit: number(&window["limit_dollars"]),
            unit: "%".into(),
        });
    }
    let extra = &value["extra_usage"];
    if extra["is_enabled"].as_bool() == Some(true)
        && let Some(percent) = number(&extra["utilization"])
    {
        let factor = 10_f64.powi(extra["decimal_places"].as_i64().unwrap_or(2).clamp(0, 8) as i32);
        result.quotas.push(Quota {
            name: "Crédits supplémentaires · mois".into(),
            used_percent: percent,
            used: number(&extra["used_credits"]).map(|n| n / factor),
            limit: number(&extra["monthly_limit"]).map(|n| n / factor),
            unit: extra["currency"].as_str().unwrap_or("crédits").into(),
            ..Default::default()
        });
    }
    result
}

fn read_incremental(path: &Path, state: &mut FileState, p: &ProviderConfig) -> Result<(), String> {
    let metadata = fs::metadata(path).map_err(|e| e.to_string())?;
    let modified = metadata.modified().ok();
    if state.kind.is_some_and(|kind| kind != p.kind)
        || metadata.len() < state.offset
        || (metadata.len() == state.offset && modified != state.modified)
    {
        *state = FileState::default();
    }
    state.kind = Some(p.kind);
    if metadata.len() == state.offset {
        return Ok(());
    }
    let mut reader = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    reader
        .seek(SeekFrom::Start(state.offset))
        .map_err(|e| e.to_string())?;
    let mut line = Vec::new();
    loop {
        line.clear();
        let length = reader
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if length == 0 {
            break;
        }
        // A running client may be writing a line. Keep the offset before it for the next pass.
        if line.last() != Some(&b'\n') && serde_json::from_slice::<Value>(&line).is_err() {
            break;
        }
        state.offset += length as u64;
        if line.len() > 16 * 1024 * 1024 {
            state.invalid += 1;
            continue;
        }
        if matches!(p.kind, ProviderKind::Codex)
            && ![b"token_count".as_slice(), b"turn_context", b"session_meta"]
                .iter()
                .any(|word| memchr::memmem::find(&line, word).is_some())
        {
            continue;
        }
        if p.kind == ProviderKind::ClaudeCode
            && memchr::memmem::find(&line, b"\"assistant\"").is_none()
        {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            if !line.iter().all(u8::is_ascii_whitespace) {
                state.invalid += 1;
            }
            continue;
        };
        match p.kind {
            ProviderKind::Codex => parse_codex(&value, state, p),
            ProviderKind::ClaudeCode => parse_claude(&value, state, p),
            _ => match serde_json::from_value::<UsageEvent>(value) {
                Ok(mut event) if event.valid() => {
                    if p.id != "manual" {
                        event.provider = p.id.clone();
                    }
                    state
                        .events
                        .insert(format!("{}:{}", event.provider, event.id), event);
                }
                _ => state.invalid += 1,
            },
        }
    }
    state.modified = modified;
    Ok(())
}

fn parse_codex(value: &Value, state: &mut FileState, p: &ProviderConfig) {
    let payload = &value["payload"];
    match value["type"].as_str() {
        Some("session_meta") => {
            state.session = payload["id"].as_str().unwrap_or("").into();
        }
        Some("turn_context") => {
            if let Some(model) = payload["model"].as_str() {
                state.model = model.into();
            }
        }
        Some("event_msg") if payload["type"] == "token_count" => {
            let Some(time) = parse_time(&value["timestamp"]) else {
                return;
            };
            if payload["rate_limits"].is_object() {
                state.quota = Some(codex_quota(&payload["rate_limits"], p, time, false));
            }
            let totals = &payload["info"]["total_token_usage"];
            if !totals.is_object() {
                return;
            }
            let current = Counts::codex(totals);
            let delta = if current.total() < state.previous.total() {
                let last = &payload["info"]["last_token_usage"];
                if last.is_object() {
                    Counts::codex(last)
                } else {
                    current
                }
            } else {
                current.delta(state.previous)
            };
            state.previous = current;
            if delta.total() == 0 {
                return;
            }
            let event = UsageEvent {
                id: format!(
                    "codex:{}:{}:{}",
                    state.session,
                    time.to_rfc3339(),
                    current.total()
                ),
                provider: p.id.clone(),
                timestamp: time,
                model: if state.model.is_empty() {
                    "Codex · modèle inconnu".into()
                } else {
                    state.model.clone()
                },
                input_tokens: delta.input,
                output_tokens: delta.output,
                cached_tokens: delta.cached.min(delta.input),
                cache_write_tokens: delta.write.min(delta.input),
                cost_usd: None,
                credits: None,
                source: "Journal Codex".into(),
                tokens_reported: true,
                request_count: 1,
            };
            state.events.insert(event.id.clone(), event);
        }
        _ => {}
    }
}

fn parse_claude(value: &Value, state: &mut FileState, p: &ProviderConfig) {
    if value["type"] != "assistant" {
        return;
    }
    let msg = &value["message"];
    let u = &msg["usage"];
    let Some(time) = parse_time(&value["timestamp"]) else {
        return;
    };
    let input = n(u, "input_tokens");
    let output = n(u, "output_tokens");
    let cached = n(u, "cache_read_input_tokens");
    let write = n(u, "cache_creation_input_tokens");
    if input + output + cached + write == 0 {
        return;
    }
    let id = msg["id"]
        .as_str()
        .or_else(|| value["uuid"].as_str())
        .unwrap_or("");
    if id.is_empty() {
        state.invalid += 1;
        return;
    }
    let request = value["requestId"].as_str().unwrap_or("");
    let event = UsageEvent {
        id: format!("claude:{id}:{request}"),
        provider: p.id.clone(),
        timestamp: time,
        model: msg["model"].as_str().unwrap_or("Claude").into(),
        input_tokens: input + cached + write,
        output_tokens: output,
        cached_tokens: cached,
        cache_write_tokens: write,
        cost_usd: None,
        credits: None,
        source: "Journal Claude Code".into(),
        tokens_reported: true,
        request_count: 1,
    };
    match state.events.get_mut(&event.id) {
        Some(old) => merge_counts(old, &event),
        None => {
            state.events.insert(event.id.clone(), event);
        }
    }
}
fn merge_counts(old: &mut UsageEvent, new: &UsageEvent) {
    old.input_tokens = old.input_tokens.max(new.input_tokens);
    old.output_tokens = old.output_tokens.max(new.output_tokens);
    old.cached_tokens = old.cached_tokens.max(new.cached_tokens);
    old.cache_write_tokens = old.cache_write_tokens.max(new.cache_write_tokens);
    old.timestamp = old.timestamp.min(new.timestamp);
    old.cost_usd = old.cost_usd.or(new.cost_usd);
    old.credits = old.credits.or(new.credits);
    old.tokens_reported |= new.tokens_reported;
    old.request_count = old.request_count.max(new.request_count);
}
fn merge_event(events: &mut HashMap<(String, String), UsageEvent>, event: UsageEvent) {
    let key = (event.provider.clone(), event.id.clone());
    match events.get_mut(&key) {
        Some(old) => merge_counts(old, &event),
        None => {
            events.insert(key, event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn ollama_legacy_remaining_is_converted_to_used_with_real_resets() {
        let p = ProviderConfig::default();
        let report = ollama_balance(
            &json!({"included":{"session":{"remaining_percent":96.79,"resets_at":"2026-11-02T19:00:00Z"},"weekly":{"remaining_percent":81.49,"resets_at":"2026-11-09T00:00:00Z"}},"purchased":{"balance_usd":0}}),
            &p,
            Utc::now(),
        );
        assert_eq!(report.quotas.len(), 2);
        assert!((report.quotas[0].used_percent - 3.21).abs() < 0.0001);
        assert!((report.quotas[1].used_percent - 18.51).abs() < 0.0001);
        assert_eq!(
            report.quotas[0].resets_at.unwrap().to_rfc3339(),
            "2026-11-02T19:00:00+00:00"
        );
    }
    #[test]
    fn ollama_monthly_allowance_and_purchased_balance_stay_distinct() {
        let p = ProviderConfig::default();
        let report = ollama_balance(
            &json!({"included":{"balance_usd":44.75,"allowance_usd":60,"period":{"from":"2026-11-01T00:00:00Z","until":"2026-12-01T00:00:00Z"}},"purchased":{"balance_usd":12.5}}),
            &p,
            Utc::now(),
        );
        assert_eq!(report.quotas.len(), 1);
        assert_eq!(report.quotas[0].used, Some(15.25));
        assert_eq!(report.credit_balance.as_deref(), Some("12.50 USD"));
    }
    #[test]
    fn ollama_requests_do_not_become_invented_tokens() {
        let p = ProviderConfig::default();
        let events = ollama_events(
            &json!({"buckets":[{"from":"2026-11-02T00:00:00Z","request_count":10,"groups":[{"model":"glm","request_count":8}],"remainder":{"request_count":2}}]}),
            &p,
        );
        assert_eq!(events.iter().map(|e| e.request_count).sum::<u64>(), 10);
        assert!(events.iter().all(|e| !e.tokens_reported));
        assert_eq!(events.iter().map(UsageEvent::total).sum::<u64>(), 0);
        let events = ollama_events(
            &json!({"buckets":[{"from":"2026-11-02T00:00:00Z","groups":[{"model":"glm","request_count":1,"input_tokens":100,"cached_input_tokens":80,"output_tokens":10,"usage_usd":0.02}]}]}),
            &p,
        );
        assert!(events[0].tokens_reported);
        assert_eq!(events[0].total(), 110);
        assert_eq!(events[0].cost_usd, Some(0.02));
    }
    #[test]
    fn cumulative_codex_and_duplicate_snapshots() {
        let p = ProviderConfig {
            id: "codex".into(),
            kind: ProviderKind::Codex,
            ..Default::default()
        };
        let mut state = FileState::default();
        for (time, input, output) in [
            ("2026-11-01T00:00:00Z", 100, 10),
            ("2026-11-01T00:00:01Z", 100, 10),
            ("2026-11-01T00:00:02Z", 150, 20),
        ] {
            parse_codex(
                &json!({"type":"event_msg","timestamp":time,"payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":input,"output_tokens":output,"cached_input_tokens":50}}}}),
                &mut state,
                &p,
            );
        }
        assert_eq!(state.events.len(), 2);
        assert_eq!(
            state.events.values().map(UsageEvent::total).sum::<u64>(),
            170
        );
        assert_eq!(
            state.events.values().map(|e| e.cached_tokens).sum::<u64>(),
            50
        );
    }
    #[test]
    fn claude_streaming_parts_merge_and_cache_is_counted_once() {
        let p = ProviderConfig::default();
        let mut state = FileState::default();
        for out in [5, 20, 20] {
            parse_claude(
                &json!({"type":"assistant","timestamp":"2026-11-01T00:00:00Z","message":{"id":"msg1","model":"claude","usage":{"input_tokens":10,"output_tokens":out,"cache_read_input_tokens":100,"cache_creation_input_tokens":30}}}),
                &mut state,
                &p,
            );
        }
        assert_eq!(state.events.len(), 1);
        let e = state.events.values().next().unwrap();
        assert_eq!(e.total(), 160);
        assert_eq!(e.cached_tokens, 100);
    }
    #[test]
    fn quota_null_reset_and_distinct_shared_windows() {
        let p = ProviderConfig::default();
        let q = claude_quota(
            &json!({"five_hour":{"utilization":0,"resets_at":null},"seven_day":{"utilization":79,"resets_at":"2026-11-05T11:59:59Z"},"seven_day_opus":null}),
            &p,
            Utc::now(),
        );
        assert_eq!(q.quotas.len(), 2);
        assert!(q.quotas[0].resets_at.is_none());
        assert_eq!(q.quotas[1].used_percent, 79.0);
    }
    #[test]
    fn incomplete_append_is_read_on_the_next_scan() {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("{}.jsonl", uuid::Uuid::new_v4()));
        let p = ProviderConfig::default();
        let mut state = FileState::default();
        let text =
            r#"{"id":"x","provider":"p","timestamp":"2026-11-01T00:00:00Z","input_tokens":100}"#;
        fs::write(&path, &text[..40]).unwrap();
        read_incremental(&path, &mut state, &p).unwrap();
        assert_eq!(state.offset, 0);
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{}", &text[40..]).unwrap();
        read_incremental(&path, &mut state, &p).unwrap();
        assert_eq!(state.events.len(), 1);
        fs::remove_file(path).unwrap();
    }
}
