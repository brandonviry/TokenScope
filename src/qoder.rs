//! Qoder CLI SDK: local, opt-in and read-only quota collection.
use crate::model::{ProviderConfig, ProviderSnapshot, Quota};
use chrono::Utc;
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::Command,
};

const TIMEOUT: Duration = Duration::from_secs(12);

fn executable() -> PathBuf {
    if let Some(path) = std::env::var_os("QODERCLI_PATH").filter(|v| !v.is_empty()) {
        return path.into();
    }
    if let Some(home) = directories::UserDirs::new() {
        let path = home.home_dir().join(".qoder/bin/qodercli/qodercli.exe");
        if path.is_file() {
            return path;
        }
    }
    PathBuf::from("qodercli")
}

async fn request<W: AsyncWrite + Unpin, R: AsyncBufRead + Unpin>(
    writer: &mut W,
    reader: &mut R,
    id: &str,
    request: Value,
) -> Result<Value, String> {
    let mut message =
        serde_json::to_vec(&json!({"type":"control_request", "request_id":id, "request":request}))
            .map_err(|_| "Requête Qoder invalide.")?;
    message.push(b'\n');
    writer
        .write_all(&message)
        .await
        .map_err(|_| "Impossible de communiquer avec Qoder CLI.")?;
    writer
        .flush()
        .await
        .map_err(|_| "Impossible de communiquer avec Qoder CLI.")?;
    loop {
        let mut line = Vec::new();
        let n = (&mut *reader)
            .take(1024 * 1024 + 1)
            .read_until(b'\n', &mut line)
            .await
            .map_err(|_| "Lecture de Qoder CLI impossible.")?;
        if n == 0 {
            return Err("Qoder CLI s’est arrêté avant de répondre.".into());
        }
        if n > 1024 * 1024 {
            return Err("Réponse Qoder trop volumineuse.".into());
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if value["type"] != "control_response" || value["response"]["request_id"] != id {
            continue;
        }
        return match value["response"]["subtype"].as_str() {
            Some("success") => Ok(value["response"]["response"].clone()),
            Some("error") => Err("Qoder CLI a refusé la demande. Vérifiez la connexion et la compatibilité de sa version.".into()),
            _ => Err("Réponse Qoder non reconnue.".into()),
        };
    }
}

async fn collect(path: PathBuf) -> Result<Value, String> {
    let mut cmd = Command::new(path);
    cmd.arg("--sdk")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x08000000);
    let mut child = cmd.spawn().map_err(|_| {
        "Qoder CLI introuvable ou impossible à lancer. Installez-le puis connectez-vous."
            .to_string()
    })?;
    let mut stdin = child.stdin.take().ok_or("Entrée Qoder CLI indisponible.")?;
    let mut stdout = BufReader::new(
        child
            .stdout
            .take()
            .ok_or("Sortie Qoder CLI indisponible.")?,
    );
    let result = tokio::time::timeout(TIMEOUT, async {
        request(&mut stdin, &mut stdout, "tokenscope-init", json!({"subtype":"daemon.initialize","daemon_protocol_version":2,"sdk_version":"0.1.0","capabilities":["multi_session_v2"],"auth":{"type":"qodercli"}})).await?;
        request(&mut stdin, &mut stdout, "tokenscope-usage", json!({"subtype":"daemon.get_usage_info"})).await
    }).await.unwrap_or_else(|_| Err("Qoder CLI ne répond pas. Dernier relevé conservé.".into()));
    drop(stdin);
    let _ = child.kill().await;
    let _ = child.wait().await;
    result
}

pub fn fetch(provider: &ProviderConfig) -> Result<ProviderSnapshot, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Impossible de démarrer le connecteur Qoder.")?;
    let value = runtime.block_on(collect(executable()))?;
    parse(&value, provider)
}

fn parse(value: &Value, provider: &ProviderConfig) -> Result<ProviderSnapshot, String> {
    let usage = value
        .get("usage")
        .and_then(Value::as_object)
        .ok_or("Qoder n’a pas fourni de quotas exploitables.")?;
    let mut report = ProviderSnapshot {
        id: provider.id.clone(),
        observed_at: Some(Utc::now()),
        source: "Qoder CLI".into(),
        status: "Quotas Qoder synchronisés".into(),
        live: true,
        usage_note: Some("Crédits Qoder · historique de tokens non fourni".into()),
        ..Default::default()
    };
    for (key, name) in [
        ("userQuota", "Crédits du forfait"),
        ("addOnQuota", "Crédits supplémentaires"),
    ] {
        let Some(quota) = usage.get(key) else {
            continue;
        };
        let Some(total) = quota["total"]
            .as_f64()
            .filter(|n| n.is_finite() && *n > 0.0)
        else {
            continue;
        };
        let Some(used) = quota["used"]
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0 && *n <= total)
        else {
            continue;
        };
        if quota["unit"].as_str() != Some("credits") {
            continue;
        }
        report.quotas.push(Quota {
            name: name.into(),
            used_percent: used / total * 100.0,
            used: Some(used),
            limit: Some(total),
            unit: "crédits".into(),
            ..Default::default()
        });
    }
    if report.quotas.is_empty() {
        return Err("Qoder n’a pas fourni de quotas de crédits exploitables.".into());
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn separate_quotas_and_ignore_personal_data() {
        let v = json!({"usage":{"userId":"private","expiresAt":1796313600000_i64,"userQuota":{"total":1000,"used":20,"unit":"credits"},"addOnQuota":{"total":100,"used":25,"unit":"credits"}}});
        let r = parse(&v, &ProviderConfig::default()).unwrap();
        assert_eq!(r.quotas.len(), 2);
        assert_eq!(r.quotas[0].used_percent, 2.0);
        assert_eq!(r.quotas[1].used_percent, 25.0);
        assert!(r.quotas[0].resets_at.is_none());
        assert!(!serde_json::to_string(&r).unwrap().contains("private"));
    }
    #[test]
    fn rejects_invalid_or_missing_quotas() {
        for v in [
            json!({}),
            json!({"usage":{"userQuota":{"total":0,"used":0,"unit":"credits"}}}),
            json!({"usage":{"userQuota":{"total":10,"used":20,"unit":"credits"}}}),
        ] {
            assert!(parse(&v, &ProviderConfig::default()).is_err());
        }
    }
    #[test]
    fn protocol_matches_request_ids() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (client, server) = tokio::io::duplex(4096);
            let (mut client_read, mut client_write) = tokio::io::split(client);
            let (server_read, mut server_write) = tokio::io::split(server);
            let server_task = tokio::spawn(async move {
                let mut lines = BufReader::new(server_read).lines();
                let line = lines.next_line().await.unwrap().unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(req["request"]["subtype"], "daemon.get_usage_info");
                server_write.write_all(b"{\"type\":\"control_response\",\"response\":{\"request_id\":\"other\",\"subtype\":\"success\"}}\n{\"type\":\"control_response\",\"response\":{\"request_id\":\"wanted\",\"subtype\":\"success\",\"response\":{\"usage\":{}}}}\n").await.unwrap();
            });
            let result = request(&mut client_write, &mut BufReader::new(&mut client_read), "wanted", json!({"subtype":"daemon.get_usage_info"})).await.unwrap();
            assert!(result["usage"].is_object());
            server_task.await.unwrap();
        });
    }
    #[test]
    fn protocol_handles_rejection_and_closed_output() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            for bytes in [b"{\"type\":\"control_response\",\"response\":{\"request_id\":\"id\",\"subtype\":\"error\",\"error\":\"secret\"}}\n".as_slice(), b"".as_slice()] {
                let mut reader = BufReader::new(bytes);
                let error = request(&mut tokio::io::sink(), &mut reader, "id", json!({})).await.unwrap_err();
                assert!(!error.contains("secret"));
            }
        });
    }
}
