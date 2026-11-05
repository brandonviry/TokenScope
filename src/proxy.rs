//! Optional native Ollama relay. No UI server, no inference of its own.
//! Only numerical usage from completed responses is persisted.
use crate::{model::UsageEvent, storage::Store};
use chrono::Utc;
use serde_json::Value;
use std::{
    io::{self, Read},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

pub struct ProxyHandle {
    stop: Arc<AtomicBool>,
    pub status: Arc<Mutex<String>>,
    join: Option<thread::JoinHandle<()>>,
}
impl Drop for ProxyHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl ProxyHandle {
    pub fn start(
        port: u16,
        upstream: &str,
        store: Store,
        provider: String,
    ) -> Result<Self, String> {
        let url =
            reqwest::Url::parse(upstream).map_err(|_| "Adresse Ollama invalide.".to_string())?;
        if url.scheme() != "http"
            || !matches!(
                url.host_str(),
                Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
            )
        {
            return Err(
                "Le collecteur accepte un serveur Ollama local en HTTP (localhost).".into(),
            );
        }
        if url.port_or_known_default() == Some(port) {
            return Err("Le port du collecteur doit différer de celui d’Ollama.".into());
        }
        let server = tiny_http::Server::http(("127.0.0.1", port))
            .map_err(|e| format!("Port {port} indisponible : {e}"))?;
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(1800))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        let status = Arc::new(Mutex::new(format!("Actif sur 127.0.0.1:{port}")));
        let stop = Arc::new(AtomicBool::new(false));
        let mut handle = Self {
            stop: stop.clone(),
            status: status.clone(),
            join: None,
        };
        let base = upstream.trim_end_matches('/').to_string();
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        handle.join = Some(
            thread::Builder::new()
                .name("ollama-collector".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let request = match server.recv_timeout(Duration::from_millis(200)) {
                            Ok(Some(r)) => r,
                            Ok(None) => continue,
                            Err(_) => break,
                        };
                        if active.load(Ordering::Relaxed) >= 16 {
                            let _ = request.respond(
                                tiny_http::Response::from_string("Collecteur occupé")
                                    .with_status_code(503),
                            );
                            continue;
                        }
                        active.fetch_add(1, Ordering::Relaxed);
                        let (client, base, store, provider, status, active) = (
                            client.clone(),
                            base.clone(),
                            store.clone(),
                            provider.clone(),
                            status.clone(),
                            active.clone(),
                        );
                        thread::spawn(move || {
                            relay(request, &client, &base, store, provider, status);
                            active.fetch_sub(1, Ordering::Relaxed);
                        });
                    }
                })
                .map_err(|e| e.to_string())?,
        );
        Ok(handle)
    }
    pub fn status(&self) -> String {
        self.status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| "État indisponible".into())
    }
}

fn relay(
    mut request: tiny_http::Request,
    client: &reqwest::blocking::Client,
    base: &str,
    store: Store,
    provider: String,
    status: Arc<Mutex<String>>,
) {
    let path = request.url().to_string();
    // Never allow absolute URLs, administration, deletion, model downloads or pushes.
    if !matches!(request.method().as_str(), "POST" | "GET" | "HEAD")
        || ![
            "/api/chat",
            "/api/generate",
            "/api/tags",
            "/api/ps",
            "/api/version",
            "/api/show",
            "/api/embed",
            "/api/embeddings",
            "/v1/chat/completions",
            "/v1/completions",
            "/v1/embeddings",
            "/v1/models",
        ]
        .iter()
        .any(|allowed| path.split('?').next() == Some(*allowed))
    {
        let _ = request.respond(
            tiny_http::Response::from_string("Route Ollama non prise en charge")
                .with_status_code(404),
        );
        return;
    }
    let mut body = vec![];
    if request
        .as_reader()
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut body)
        .is_err()
        || body.len() > 16 * 1024 * 1024
    {
        let _ = request.respond(
            tiny_http::Response::from_string("Requête trop volumineuse").with_status_code(413),
        );
        return;
    }
    let model = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v["model"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "Ollama".into());
    let method = reqwest::Method::from_bytes(request.method().as_str().as_bytes())
        .unwrap_or(reqwest::Method::GET);
    let mut outgoing = client.request(method, format!("{base}{path}")).body(body);
    for header in request.headers() {
        let name = header.field.to_string();
        if ![
            "host",
            "content-length",
            "transfer-encoding",
            "connection",
            "authorization",
            "cookie",
        ]
        .contains(&name.to_ascii_lowercase().as_str())
        {
            outgoing = outgoing.header(name, header.value.as_str());
        }
    }
    let response = match outgoing.send() {
        Ok(r) => r,
        Err(_) => {
            set_status(
                &status,
                "Ollama inaccessible. Vérifiez que le serveur est démarré.",
            );
            let _ = request.respond(
                tiny_http::Response::from_string("Ollama inaccessible").with_status_code(502),
            );
            return;
        }
    };
    let code = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter(|(name, _)| {
            !["content-length", "transfer-encoding", "connection"].contains(&name.as_str())
        })
        .filter_map(|(k, v)| tiny_http::Header::from_bytes(k.as_str(), v.as_bytes()).ok())
        .collect();
    let capture = ResponseCapture::new(model, provider);
    let tee = CaptureReader {
        reader: response,
        capture,
        store,
        status: status.clone(),
        success: (200..300).contains(&code),
    };
    if request
        .respond(tiny_http::Response::new(
            tiny_http::StatusCode(code),
            headers,
            tee,
            None,
            None,
        ))
        .is_err()
    {
        set_status(&status, "Client déconnecté avant la fin de la réponse.");
    }
}

fn set_status(status: &Arc<Mutex<String>>, message: &str) {
    if let Ok(mut s) = status.lock() {
        *s = message.into();
    }
}

struct CaptureReader {
    reader: reqwest::blocking::Response,
    capture: ResponseCapture,
    store: Store,
    status: Arc<Mutex<String>>,
    success: bool,
}
impl Read for CaptureReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let length = self.reader.read(buffer)?;
        if self.success {
            self.capture.feed(&buffer[..length]);
        }
        Ok(length)
    }
}
impl Drop for CaptureReader {
    fn drop(&mut self) {
        self.capture.finish();
        if let Some(event) = self.capture.event.take() {
            match self.store.append_event(&event, true) {
                Ok(()) => set_status(
                    &self.status,
                    &format!(
                        "Dernier appel capturé : {} tokens · {}",
                        event.total(),
                        Utc::now().format("%H:%M:%S UTC")
                    ),
                ),
                Err(e) => set_status(&self.status, &format!("Enregistrement impossible : {e}")),
            }
        }
    }
}

struct ResponseCapture {
    buffer: Vec<u8>,
    model: String,
    provider: String,
    event: Option<UsageEvent>,
    id: String,
}
impl ResponseCapture {
    fn new(model: String, provider: String) -> Self {
        Self {
            buffer: vec![],
            model,
            provider,
            event: None,
            id: uuid::Uuid::new_v4().to_string(),
        }
    }
    fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' {
                self.consume();
                self.buffer.clear();
            } else if self.buffer.len() < 1024 * 1024 {
                self.buffer.push(byte);
            }
        }
    }
    fn finish(&mut self) {
        self.consume();
        self.buffer.clear();
    }
    fn consume(&mut self) {
        let line = self.buffer.strip_prefix(b"data: ").unwrap_or(&self.buffer);
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        let (input, output, cached) =
            if value["done"] == true && value["prompt_eval_count"].is_number() {
                (
                    value["prompt_eval_count"].as_u64().unwrap_or(0),
                    value["eval_count"].as_u64().unwrap_or(0),
                    value["prompt_eval_cached_count"].as_u64().unwrap_or(0),
                )
            } else if value["usage"]["prompt_tokens"].is_number() {
                (
                    value["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
                    value["usage"]["completion_tokens"].as_u64().unwrap_or(0),
                    value["usage"]["prompt_tokens_details"]["cached_tokens"]
                        .as_u64()
                        .unwrap_or(0),
                )
            } else {
                return;
            };
        self.event = Some(UsageEvent {
            id: format!("ollama:{}", self.id),
            provider: self.provider.clone(),
            timestamp: Utc::now(),
            model: value["model"].as_str().unwrap_or(&self.model).into(),
            input_tokens: input,
            output_tokens: output,
            cached_tokens: cached.min(input),
            cache_write_tokens: 0,
            cost_usd: Some(0.0),
            credits: None,
            source: "Collecteur Ollama local".into(),
            tokens_reported: true,
            request_count: 1,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streaming_chunks_only_capture_final_metrics() {
        let mut c = ResponseCapture::new("llama".into(), "llama".into());
        c.feed(b"{\"response\":\"bonjour\",\"done\":false}\n{\"done\":true,\"prompt_eval_co");
        assert!(c.event.is_none());
        c.feed(b"unt\":100,\"eval_count\":12,\"prompt_eval_cached_count\":70}\n");
        let e = c.event.as_ref().unwrap();
        assert_eq!(e.total(), 112);
        assert_eq!(e.cached_tokens, 70);
        let text = serde_json::to_string(e).unwrap();
        assert!(!text.contains("bonjour"));
    }
    #[test]
    fn openai_sse_and_non_streaming_are_supported() {
        let mut c = ResponseCapture::new("llama".into(), "llama".into());
        c.feed(
            b"data: {\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":4}}\n\ndata: [DONE]\n",
        );
        assert_eq!(c.event.unwrap().total(), 12);
    }
    #[test]
    fn relay_forwards_a_real_response_and_persists_usage() {
        let upstream = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let upstream_port = upstream.server_addr().to_ip().unwrap().port();
        let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = reserve.local_addr().unwrap().port();
        drop(reserve);
        let root = std::env::temp_dir().join(format!("tokenscope-proxy-{}", uuid::Uuid::new_v4()));
        let store = Store::new(Some(root.clone()));
        let read_store = store.clone();
        let worker = thread::spawn(move || {
            let request = upstream
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            assert_eq!(request.url(), "/api/generate");
            request
                .respond(tiny_http::Response::from_string(
                    "{\"done\":true,\"prompt_eval_count\":10,\"eval_count\":5}\n",
                ))
                .unwrap();
        });
        let handle = ProxyHandle::start(
            port,
            &format!("http://127.0.0.1:{upstream_port}"),
            store,
            "llama".into(),
        )
        .unwrap();
        let response = reqwest::blocking::Client::new()
            .post(format!("http://127.0.0.1:{port}/api/generate"))
            .body("{\"model\":\"llama\"}")
            .send()
            .unwrap()
            .text()
            .unwrap();
        assert!(response.contains("prompt_eval_count"));
        worker.join().unwrap();
        for _ in 0..50 {
            if read_store.ollama_path().exists() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let event: UsageEvent = serde_json::from_str(
            std::fs::read_to_string(read_store.ollama_path())
                .unwrap()
                .trim(),
        )
        .unwrap();
        assert_eq!(event.total(), 15);
        drop(handle);
        std::fs::remove_dir_all(root).unwrap();
    }
}
