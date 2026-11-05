//! Read-only Antigravity quota integration. Reuses the official client's
//! existing OAuth session; never copies refresh tokens into TokenScope data.
use crate::model::{ProviderConfig, ProviderSnapshot, Quota};
use chrono::{DateTime, Duration, Utc};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration as StdDuration, Instant},
};

const ENDPOINTS: [&str; 2] = [
    "https://daily-cloudcode-pa.googleapis.com",
    "https://cloudcode-pa.googleapis.com",
];
// This product header selects Antigravity quota, rather than Gemini CLI / Code
// Assist. The API serves different entitlements for the same OAuth account.
const USER_AGENT: &str = "antigravity/1.4.0 TokenScope/1.1.0 windows/amd64";
const CLIENT_METADATA: &str =
    r#"{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}"#;

struct Session {
    access_token: String,
    expiry: Option<DateTime<Utc>>,
    native: bool,
    refreshable: bool,
    source: String,
}
impl Session {
    fn expiring(&self) -> bool {
        self.expiry
            .is_some_and(|expiry| expiry <= Utc::now() + Duration::seconds(60))
    }
}

fn expiry(value: &Value) -> Option<DateTime<Utc>> {
    value
        .as_str()
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .map(|t| t.with_timezone(&Utc))
        .or_else(|| value.as_i64().and_then(DateTime::from_timestamp_millis))
}
fn parse_session(value: &Value, source: String, native: bool) -> Option<Session> {
    let token = value
        .get("token")
        .filter(|t| t.is_object())
        .unwrap_or(value);
    if let Some(tokens) = value["tokens"].as_array() {
        return tokens
            .iter()
            .filter_map(|t| parse_session(t, source.clone(), false))
            .find(|s| !s.expiring());
    }
    let access = token["access_token"]
        .as_str()
        .or_else(|| token["accessToken"].as_str())?
        .trim();
    if access.is_empty() {
        return None;
    }
    let expiration = ["expiry", "expiry_date", "expires_at", "expiresAt"]
        .iter()
        .find_map(|key| expiry(&token[*key]));
    Some(Session {
        access_token: access.into(),
        expiry: expiration,
        native,
        refreshable: token["refresh_token"]
            .as_str()
            .is_some_and(|v| !v.is_empty()),
        source,
    })
}
fn decode_session(bytes: &[u8], source: String, native: bool) -> Option<Session> {
    if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
        return parse_session(&value, source, native);
    }
    // Some Windows clients persist credential blobs as UTF-16LE.
    if bytes.len().is_multiple_of(2) {
        let words: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        if let Ok(text) = String::from_utf16(&words)
            && let Ok(value) = serde_json::from_str::<Value>(text.trim_matches(['\u{feff}', '\0']))
        {
            return parse_session(&value, source, native);
        }
    }
    None
}
fn file_candidates() -> Vec<PathBuf> {
    let Some(user) = directories::UserDirs::new() else {
        return vec![];
    };
    let base = user.home_dir().join(".gemini");
    [
        "antigravity-cli/antigravity-oauth-token",
        "antigravity-ide/antigravity-oauth-token",
        "antigravity/antigravity-oauth-token",
        "antigravity-cli/agy-hud-token.json",
    ]
    .iter()
    .map(|relative| base.join(relative))
    .collect()
}
fn native_session() -> Option<Session> {
    #[cfg(windows)]
    for target in [
        "gemini:antigravity",
        "LegacyGeneric:target=gemini:antigravity",
    ] {
        let Some(bytes) = keyring::Entry::new_with_target(target, "gemini", "antigravity")
            .ok()
            .and_then(|entry| entry.get_secret().ok())
        else {
            continue;
        };
        if let Some(session) = decode_session(&bytes, "Coffre Windows · Antigravity".into(), true)
        {
            return Some(session);
        }
    }
    None
}
fn find_session(provider: &ProviderConfig) -> Result<Session, String> {
    if !provider.auth_path.trim().is_empty() {
        let bytes = fs::read(provider.auth_path.trim())
            .map_err(|_| "Fichier de session Antigravity introuvable ou illisible.".to_string())?;
        return decode_session(&bytes, "Fichier de session Antigravity".into(), false)
            .ok_or_else(|| "Format de session Antigravity non reconnu.".into());
    }
    if let Some(session) = native_session() {
        return Ok(session);
    }
    for path in file_candidates() {
        if let Some(session) = fs::read(path)
            .ok()
            .and_then(|bytes| decode_session(&bytes, "Session locale Antigravity".into(), false))
        {
            return Ok(session);
        }
    }
    Err("Connexion Antigravity introuvable. Connectez-vous dans Antigravity ou agy, puis actualisez.".into())
}
pub fn connection_description(provider: &ProviderConfig) -> String {
    match find_session(provider) {
        Ok(session) => format!(
            "{} · {}",
            session.source,
            if session.expiring() {
                "session à renouveler"
            } else {
                "session détectée"
            }
        ),
        Err(error) => error,
    }
}
fn refresh_native_session() -> Result<(), String> {
    let Some(user) = directories::UserDirs::new() else {
        return Err("Dossier utilisateur introuvable.".into());
    };
    let executable = [
        user.home_dir().join("AppData/Local/agy/bin/agy.exe"),
        user.home_dir().join(".local/bin/agy"),
    ]
    .into_iter()
    .find(|p| p.is_file())
    .ok_or("Session Antigravity expirée. Ouvrez le client officiel pour la renouveler.")?;
    // `models` only lists the catalog. It lets the official native client
    // renew its own session without submitting a prompt or generating tokens.
    let mut command = Command::new(executable);
    command
        .arg("models")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().map_err(|_| {
        "Impossible de renouveler la session avec le client Antigravity.".to_string()
    })?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(
                        "La session Antigravity doit être reconnectée dans le client officiel."
                            .into(),
                    )
                };
            }
            Ok(None) if started.elapsed() < StdDuration::from_secs(15) => {
                thread::sleep(StdDuration::from_millis(100))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Renouvellement Antigravity trop long. Ouvrez le client officiel puis actualisez.".into());
            }
        }
    }
}

fn request(
    client: &Client,
    endpoint: &str,
    operation: &str,
    body: &Value,
    token: &str,
) -> Result<Value, String> {
    let response = client
        .post(format!("{endpoint}/v1internal:{operation}"))
        .bearer_auth(token)
        .header("User-Agent", USER_AGENT)
        .header("Client-Metadata", CLIENT_METADATA)
        .timeout(StdDuration::from_secs(8))
        .json(body)
        .send()
        .map_err(|_| {
            "Google Antigravity inaccessible ou délai dépassé. Dernier relevé conservé.".to_string()
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            401 => "Session Google expirée. Reconnectez Antigravity puis actualisez.".into(),
            403 => "Google refuse la lecture des quotas Antigravity pour cette session.".into(),
            429 => "Google limite les lectures de quota. Nouvelle tentative automatique plus tard."
                .into(),
            code => format!("Google Antigravity indisponible (HTTP {code})."),
        });
    }
    response
        .json()
        .map_err(|_| "Réponse des quotas Antigravity non reconnue.".into())
}

pub fn fetch(client: &Client, provider: &ProviderConfig) -> Result<ProviderSnapshot, String> {
    let mut session = find_session(provider)?;
    if session.expiring() {
        if session.native && session.refreshable {
            refresh_native_session()?;
            session = find_session(provider)?;
        }
        if session.expiring() {
            return Err(
                "La session Antigravity est expirée. Reconnectez le client officiel.".into(),
            );
        }
    }
    let mut last_error = "Quotas Antigravity indisponibles.".to_string();
    for endpoint in ENDPOINTS {
        let models = match request(
            client,
            endpoint,
            "fetchAvailableModels",
            &json!({}),
            &session.access_token,
        ) {
            Ok(value) => value,
            Err(error) => {
                last_error = error;
                continue;
            }
        };
        let tier=request(client,endpoint,"loadCodeAssist",&json!({"metadata":{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"},"mode":"HEALTH_CHECK"}),&session.access_token).ok();
        let mut report = parse_quotas(&models, tier.as_ref(), provider, Utc::now())?;
        report.source = "Compte Google Antigravity".into();
        if tier.is_none() {
            report.status = "Quotas synchronisés ; forfait non communiqué par Google.".into();
        }
        return Ok(report);
    }
    Err(last_error)
}

pub fn parse_quotas(
    models: &Value,
    tier: Option<&Value>,
    provider: &ProviderConfig,
    now: DateTime<Utc>,
) -> Result<ProviderSnapshot, String> {
    let map = models["models"]
        .as_object()
        .ok_or("Catalogue de quotas Antigravity non reconnu.")?;
    let mut entries: Vec<_> = map
        .iter()
        .filter(|(id, _)| id.to_ascii_lowercase().starts_with("gemini-"))
        .collect();
    // Modern agent quotas first; utility aliases remain accessible in the
    // expandable list. Names and fractions always originate in the response.
    entries.sort_by_key(|(id, _)| {
        (
            !id.contains("tiered"),
            !id.contains("pro"),
            !id.contains("flash-image"),
            id.as_str(),
        )
    });
    let mut report = ProviderSnapshot {
        id: provider.id.clone(),
        observed_at: Some(now),
        source: "Google Antigravity".into(),
        status: "Quotas Gemini synchronisés avec Google".into(),
        live: true,
        usage_note: Some("Quota actif par modèle · tokens non fournis".into()),
        ..Default::default()
    };
    for (id, model) in entries {
        let Some(info) = model["quotaInfo"].as_object() else {
            continue;
        };
        let reset = expiry(&model["quotaInfo"]["resetTime"]);
        let remaining = match info.get("remainingFraction") {
            Some(value) => value
                .as_f64()
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v)),
            // Protobuf JSON omits zero-valued scalar fields. A reset timestamp
            // identifies an actual quota window; an empty object is unknown.
            None if reset.is_some() => Some(0.0),
            None => None,
        };
        let Some(remaining) = remaining else { continue };
        let name = model["displayName"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "Gemini {}",
                    id.trim_start_matches("gemini-")
                        .replace("-tiered", "")
                        .replace('-', " ")
                )
            });
        report.quotas.push(Quota {
            name: format!("{name} · quota actif"),
            used_percent: (1.0 - remaining) * 100.0,
            resets_at: reset,
            window_seconds: None,
            unit: "%".into(),
            ..Default::default()
        });
    }
    if report.quotas.is_empty() {
        return Err("Google n’a pas communiqué de quota Gemini exploitable pour ce compte.".into());
    }
    if let Some(tier) = tier {
        report.plan = tier
            .pointer("/paidTier/name")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .or_else(|| tier.pointer("/currentTier/name").and_then(Value::as_str))
            .map(str::to_owned);
        let credits = tier
            .pointer("/paidTier/availableCredits")
            .or_else(|| tier.pointer("/currentTier/availableCredits"));
        if let Some(credits) = credits.and_then(Value::as_array) {
            let amounts: Vec<u64> = credits
                .iter()
                .filter(|v| v["creditType"] == "GOOGLE_ONE_AI")
                .filter_map(|v| {
                    v["creditAmount"]
                        .as_u64()
                        .or_else(|| v["creditAmount"].as_str().and_then(|s| s.parse().ok()))
                })
                .collect();
            if !amounts.is_empty() {
                report.credit_balance = Some(format!(
                    "{} crédits Google AI",
                    amounts.into_iter().fold(0_u64, u64::saturating_add)
                ));
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remaining_fraction_and_actual_reset_are_preserved() {
        let provider = ProviderConfig::default();
        let now = Utc::now();
        let response = json!({"models":{"gemini-pro":{"displayName":"Gemini Pro","quotaInfo":{"remainingFraction":0.375,"resetTime":"2026-11-09T12:00:00Z"}},"claude":{"quotaInfo":{"remainingFraction":0.2}}}});
        let report = parse_quotas(&response, None, &provider, now).unwrap();
        assert_eq!(report.quotas.len(), 1);
        assert_eq!(report.quotas[0].used_percent, 62.5);
        assert!(report.quotas[0].window_seconds.is_none());
        assert!(!report.quotas[0].name.contains("hebdomadaire"));
        assert_eq!(report.observed_at, Some(now));
    }
    #[test]
    fn paid_tier_takes_precedence_over_base_free_tier() {
        let response = json!({"models":{"gemini-pro":{"quotaInfo":{"remainingFraction":1}}}});
        let tier = json!({"currentTier":{"name":"Antigravity","id":"free-tier"},"paidTier":{"name":"Google AI Pro","availableCredits":[{"creditType":"GOOGLE_ONE_AI","creditAmount":"120"},{"creditType":"GOOGLE_ONE_AI","creditAmount":"30"}]}});
        let report = parse_quotas(
            &response,
            Some(&tier),
            &ProviderConfig::default(),
            Utc::now(),
        )
        .unwrap();
        assert_eq!(report.plan.as_deref(), Some("Google AI Pro"));
        assert_eq!(
            report.credit_balance.as_deref(),
            Some("150 crédits Google AI")
        );
        assert!(report.quotas[0].resets_at.is_none());
    }
    #[test]
    fn omitted_zero_with_reset_is_exhausted_but_empty_quota_is_unknown() {
        let response = json!({"models":{"gemini-pro":{"quotaInfo":{"resetTime":"2026-11-09T12:00:00Z"}},"gemini-flash":{"quotaInfo":{}}}});
        let report = parse_quotas(&response, None, &ProviderConfig::default(), Utc::now()).unwrap();
        assert_eq!(report.quotas.len(), 1);
        assert_eq!(report.quotas[0].used_percent, 100.0);
    }
    #[test]
    fn invalid_fraction_is_not_shown_as_available() {
        for fraction in [json!(-1), json!(1.2), json!("unknown")] {
            let response =
                json!({"models":{"gemini-pro":{"quotaInfo":{"remainingFraction":fraction}}}});
            assert!(parse_quotas(&response, None, &ProviderConfig::default(), Utc::now()).is_err());
        }
    }
    #[test]
    fn reads_native_and_plain_oauth_shapes_without_retaining_refresh_tokens() {
        let native = json!({"token":{"access_token":"synthetic-access","refresh_token":"synthetic-refresh","expiry":"2026-11-09T12:00:00Z"}});
        let session = parse_session(&native, "test".into(), true).unwrap();
        assert_eq!(session.access_token, "synthetic-access");
        assert!(session.refreshable);
        let plain = json!({"access_token":"synthetic-access","expiry_date":1794225600000_i64});
        assert!(
            parse_session(&plain, "test".into(), false)
                .unwrap()
                .expiry
                .is_some()
        );
        let bytes: Vec<u8> = native
            .to_string()
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert!(decode_session(&bytes, "test".into(), true).is_some());
    }
}
