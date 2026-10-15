//! AI provider layer: a small, provider-neutral way to send a chat to a model service.
//!
//! * The **token** comes from the enterprise sign-in (done by the UI) and is kept **in memory
//!   only**: never written to disk, never serialised, never put in an error message.
//! * [`OpenAiCompatible`] speaks the common `/chat/completions` JSON + server-sent-events format
//!   with a configurable URL, model and auth header, so most gateways work without code changes.
//! * Nothing here decides *what* is sent: the context builder does, and must keep row data out
//!   unless the person allowed it.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    /// Overrides the configured model for this call.
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ChatResult {
    pub text: String,
    pub usage: Option<Usage>,
    pub finish_reason: Option<String>,
    pub cancelled: bool,
    pub elapsed_ms: u64,
}

pub enum Event {
    Delta(String),
}

/// Anything that can answer a chat. `token` is the caller's sign-in token.
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn chat(
        &self,
        req: &ChatRequest,
        token: &str,
        cancel: &AtomicBool,
        on_event: &mut dyn FnMut(Event),
    ) -> Result<ChatResult>;
}

/// Non-secret settings, saved locally. The token is not part of this.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AiConfig {
    /// `openai_compatible` or `mock` (deterministic, for demos and tests).
    pub provider: String,
    /// For example `https://ai.example.corp/v1`. `http://` is accepted for localhost only.
    pub base_url: String,
    pub path: String,
    pub model: String,
    /// Header that carries the token.
    pub auth_header: String,
    /// Prefix before the token, usually `Bearer`; empty sends the token alone.
    pub auth_scheme: String,
    /// Extra non-secret headers (tenant, API version…).
    pub extra_headers: BTreeMap<String, String>,
    pub timeout_secs: u64,
    pub stream: bool,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: "openai_compatible".into(),
            base_url: String::new(),
            path: "/chat/completions".into(),
            model: String::new(),
            auth_header: "Authorization".into(),
            auth_scheme: "Bearer".into(),
            extra_headers: BTreeMap::new(),
            timeout_secs: 120,
            stream: true,
            max_tokens: None,
            temperature: None,
        }
    }
}

fn bad_header(s: &str) -> bool {
    s.is_empty() || s.chars().any(|c| c == '\r' || c == '\n' || c == '\0')
}

impl AiConfig {
    /// Rejects settings that would leak the token or build a malformed request.
    pub fn validate(&self) -> Result<()> {
        if self.provider == "mock" {
            return Ok(());
        }
        if self.provider != "openai_compatible" {
            return Err(Error::Invalid(format!(
                "unknown AI provider {:?}",
                self.provider
            )));
        }
        let url = self.base_url.trim();
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| Error::Invalid("the endpoint must start with https://".into()))?;
        let host_port = rest.split('/').next().unwrap_or("");
        if host_port.is_empty() || host_port.contains('@') {
            return Err(Error::Invalid(
                "the endpoint needs a host and must not contain a user name or password".into(),
            ));
        }
        let host = host_port
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(host_port)
            .trim_matches(|c| c == '[' || c == ']');
        let local = matches!(host, "localhost" | "127.0.0.1" | "::1");
        match scheme {
            "https" => {}
            "http" if local => {}
            "http" => return Err(Error::Invalid("the token would travel unencrypted: use https:// (http is allowed for localhost only)".into())),
            _ => return Err(Error::Invalid("the endpoint must start with https://".into())),
        }
        if !self.path.starts_with('/') {
            return Err(Error::Invalid("the path must start with /".into()));
        }
        if self.model.trim().is_empty() {
            return Err(Error::Invalid("choose a model name".into()));
        }
        if bad_header(&self.auth_header) || self.auth_scheme.contains(['\r', '\n']) {
            return Err(Error::Invalid(
                "the authentication header is not valid".into(),
            ));
        }
        for (k, v) in &self.extra_headers {
            if bad_header(k) || v.contains(['\r', '\n']) {
                return Err(Error::Invalid(format!("header {k:?} is not valid")));
            }
            if k.eq_ignore_ascii_case(&self.auth_header) || k.eq_ignore_ascii_case("content-type") {
                return Err(Error::Invalid(format!("header {k:?} is set by the app")));
            }
        }
        if self.timeout_secs == 0 || self.timeout_secs > 3600 {
            return Err(Error::Invalid(
                "the timeout must be between 1 and 3600 seconds".into(),
            ));
        }
        Ok(())
    }

    fn url(&self) -> String {
        format!(
            "{}{}",
            self.base_url.trim().trim_end_matches('/'),
            self.path
        )
    }
}

/// Removes the token (and any `Bearer …` value) from text that may be shown or logged.
pub fn redact_token(text: &str, token: &str) -> String {
    let mut t = if token.len() >= 4 {
        text.replace(token, "***")
    } else {
        text.to_string()
    };
    let lower = t.to_ascii_lowercase();
    if let Some(i) = lower.find("bearer ") {
        let start = i + "bearer ".len();
        let end = t[start..]
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',')
            .map(|e| start + e)
            .unwrap_or(t.len());
        t.replace_range(start..end, "***");
    }
    t
}

fn short(s: &str) -> String {
    let s = s.trim();
    let mut end = s.len().min(300);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// `/chat/completions` over HTTPS with optional streaming.
pub struct OpenAiCompatible {
    cfg: AiConfig,
}

impl OpenAiCompatible {
    pub fn new(cfg: AiConfig) -> Result<Self> {
        cfg.validate()?;
        Ok(Self { cfg })
    }
}

impl Provider for OpenAiCompatible {
    fn name(&self) -> &str {
        "openai_compatible"
    }

    fn chat(
        &self,
        req: &ChatRequest,
        token: &str,
        cancel: &AtomicBool,
        on_event: &mut dyn FnMut(Event),
    ) -> Result<ChatResult> {
        let started = Instant::now();
        let model = req.model.clone().unwrap_or_else(|| self.cfg.model.clone());
        let mut body = serde_json::json!({
            "model": model,
            "stream": self.cfg.stream,
            "messages": req.messages.iter().map(|m| serde_json::json!({"role": m.role.as_str(), "content": m.content})).collect::<Vec<_>>(),
        });
        if let Some(n) = req.max_tokens.or(self.cfg.max_tokens) {
            body["max_tokens"] = n.into();
        }
        if let Some(t) = req.temperature.or(self.cfg.temperature) {
            body["temperature"] = serde_json::json!(t);
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(self.cfg.timeout_secs)))
            .http_status_as_error(false)
            .build()
            .into();
        let auth = if self.cfg.auth_scheme.trim().is_empty() {
            token.to_string()
        } else {
            format!("{} {}", self.cfg.auth_scheme.trim(), token)
        };
        let mut r = agent
            .post(&self.cfg.url())
            .header("Content-Type", "application/json")
            .header(&self.cfg.auth_header, &auth);
        if self.cfg.stream {
            r = r.header("Accept", "text/event-stream");
        }
        for (k, v) in &self.cfg.extra_headers {
            r = r.header(k, v);
        }
        let mut resp = r.send(body.to_string()).map_err(|e| {
            Error::Ai(redact_token(
                &format!("could not reach the AI service: {e}"),
                token,
            ))
        })?;
        let status = resp.status().as_u16();
        if status == 401 {
            return Err(Error::AiAuth(
                "the AI service rejected the token (expired or not valid): sign in again".into(),
            ));
        }
        if status >= 400 {
            let retry = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(|v| format!(" (retry after {v}s)"))
                .unwrap_or_default();
            let text = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(Error::Ai(redact_token(
                &format!("HTTP {status}{retry}: {}", short(&text)),
                token,
            )));
        }

        let mut out = ChatResult::default();
        if self.cfg.stream {
            let reader = BufReader::new(resp.body_mut().as_reader());
            for line in reader.lines() {
                if cancel.load(Ordering::Relaxed) {
                    out.cancelled = true;
                    break;
                }
                let line = line.map_err(|e| {
                    Error::Ai(redact_token(
                        &format!("the stream was interrupted: {e}"),
                        token,
                    ))
                })?;
                let line = line.trim();
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    break;
                }
                let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
                    continue;
                };
                if let Some(e) = v.get("error") {
                    return Err(Error::Ai(redact_token(&short(&e.to_string()), token)));
                }
                if let Some(c) = v["choices"][0]["delta"]["content"].as_str() {
                    if !c.is_empty() {
                        out.text.push_str(c);
                        on_event(Event::Delta(c.to_string()));
                    }
                }
                if let Some(f) = v["choices"][0]["finish_reason"].as_str() {
                    out.finish_reason = Some(f.to_string());
                }
                if let Some(u) = usage_of(&v) {
                    out.usage = Some(u);
                }
            }
        } else {
            let text = resp
                .body_mut()
                .read_to_string()
                .map_err(|e| Error::Ai(redact_token(&e.to_string(), token)))?;
            let v: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
                Error::Ai(redact_token(
                    &format!("unexpected answer: {}", short(&text)),
                    token,
                ))
            })?;
            let Some(c) = v["choices"][0]["message"]["content"].as_str() else {
                return Err(Error::Ai(redact_token(
                    &format!("unexpected answer shape: {}", short(&text)),
                    token,
                )));
            };
            out.text = c.to_string();
            out.finish_reason = v["choices"][0]["finish_reason"].as_str().map(String::from);
            out.usage = usage_of(&v);
            on_event(Event::Delta(out.text.clone()));
        }
        out.elapsed_ms = started.elapsed().as_millis() as u64;
        Ok(out)
    }
}

fn usage_of(v: &serde_json::Value) -> Option<Usage> {
    let u = v.get("usage")?;
    Some(Usage {
        prompt_tokens: u["prompt_tokens"].as_u64()?,
        completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
    })
}

/// Deterministic stand-in: echoes the last user message word by word.
pub struct MockProvider;

impl Provider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }
    fn chat(
        &self,
        req: &ChatRequest,
        _t: &str,
        cancel: &AtomicBool,
        on_event: &mut dyn FnMut(Event),
    ) -> Result<ChatResult> {
        let last = req
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .map(|m| m.content.as_str())
            .unwrap_or("");
        let reply = format!("mock reply to: {last}");
        let mut out = ChatResult {
            finish_reason: Some("stop".into()),
            ..Default::default()
        };
        for (i, w) in reply.split(' ').enumerate() {
            if cancel.load(Ordering::Relaxed) {
                out.cancelled = true;
                break;
            }
            let piece = if i == 0 {
                w.to_string()
            } else {
                format!(" {w}")
            };
            out.text.push_str(&piece);
            on_event(Event::Delta(piece));
        }
        Ok(out)
    }
}

#[derive(Debug, Clone)]
struct Token {
    value: String,
    expires_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiStatus {
    pub configured: bool,
    /// A token is held and has not expired.
    pub signed_in: bool,
    pub expires_at_ms: Option<i64>,
    pub provider: String,
    pub model: String,
    pub base_url: String,
    /// Why the settings are not usable, when they are not.
    pub config_error: Option<String>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Holds the settings and the in-memory token. Cheap to share between threads.
pub struct AiSession {
    cfg: Mutex<AiConfig>,
    token: Mutex<Option<Token>>,
}

/// A token this close to expiry is treated as expired so a long answer does not die half-way.
const SKEW_MS: i64 = 30_000;

impl Default for AiSession {
    fn default() -> Self {
        Self {
            cfg: Mutex::new(AiConfig::default()),
            token: Mutex::new(None),
        }
    }
}

impl AiSession {
    pub fn new(cfg: AiConfig) -> Self {
        Self {
            cfg: Mutex::new(cfg),
            token: Mutex::new(None),
        }
    }

    pub fn config(&self) -> AiConfig {
        self.cfg.lock().unwrap().clone()
    }

    pub fn set_config(&self, cfg: AiConfig) -> Result<()> {
        cfg.validate()?;
        *self.cfg.lock().unwrap() = cfg;
        Ok(())
    }

    /// Called by the UI after the enterprise sign-in. `expires_at_ms` is a Unix time in milliseconds.
    pub fn set_token(&self, value: &str, expires_at_ms: Option<i64>) -> Result<()> {
        let value = value.trim();
        if value.is_empty() || value.contains(['\r', '\n', '\0']) {
            return Err(Error::Invalid("the token is empty or not valid".into()));
        }
        *self.token.lock().unwrap() = Some(Token {
            value: value.to_string(),
            expires_at_ms,
        });
        Ok(())
    }

    pub fn clear_token(&self) {
        *self.token.lock().unwrap() = None;
    }

    fn token_at(&self, now: i64) -> Option<String> {
        let g = self.token.lock().unwrap();
        let t = g.as_ref()?;
        match t.expires_at_ms {
            Some(e) if e - SKEW_MS <= now => None,
            _ => Some(t.value.clone()),
        }
    }

    pub fn status(&self) -> AiStatus {
        let cfg = self.config();
        let err = cfg.validate().err().map(|e| e.to_string());
        AiStatus {
            configured: err.is_none(),
            signed_in: self.token_at(now_ms()).is_some(),
            expires_at_ms: self
                .token
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|t| t.expires_at_ms),
            provider: cfg.provider.clone(),
            model: cfg.model.clone(),
            base_url: cfg.base_url.clone(),
            config_error: err,
        }
    }

    fn provider(&self) -> Result<Box<dyn Provider>> {
        let cfg = self.config();
        match cfg.provider.as_str() {
            "mock" => Ok(Box::new(MockProvider)),
            _ => Ok(Box::new(OpenAiCompatible::new(cfg)?)),
        }
    }

    /// Sends one chat. Fails fast with [`Error::AiAuth`] when there is no valid token, and drops the
    /// token when the service rejects it so the next call asks for a new sign-in.
    pub fn chat(
        &self,
        req: &ChatRequest,
        cancel: &AtomicBool,
        on_event: &mut dyn FnMut(Event),
    ) -> Result<ChatResult> {
        let provider = self.provider()?;
        let token = self.token_at(now_ms()).ok_or_else(|| {
            let had = self.token.lock().unwrap().is_some();
            Error::AiAuth(if had {
                "the sign-in has expired".into()
            } else {
                "you are not signed in to the AI service".into()
            })
        });
        let token = match (token, self.config().provider.as_str()) {
            (Ok(t), _) => t,
            (Err(_), "mock") => String::new(),
            (Err(e), _) => return Err(e),
        };
        let r = provider.chat(req, &token, cancel, on_event);
        if matches!(r, Err(Error::AiAuth(_))) {
            self.clear_token();
        }
        r
    }

    /// Cheap round trip to check endpoint, model and token together.
    pub fn test(&self) -> Result<ChatResult> {
        let req = ChatRequest {
            messages: vec![ChatMessage {
                role: Role::User,
                content: "Reply with the single word OK.".into(),
            }],
            max_tokens: Some(8),
            ..Default::default()
        };
        self.chat(&req, &AtomicBool::new(false), &mut |_| {})
    }
}

pub type SharedAi = Arc<AiSession>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    struct Fake {
        port: u16,
        requests: Arc<Mutex<Vec<(String, String)>>>,
    }

    /// Serves one canned raw HTTP response per connection, in order, and records each request.
    fn serve(responses: Vec<String>) -> Fake {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let rec = requests.clone();
        std::thread::spawn(move || {
            for resp in responses {
                let Ok((mut s, _)) = l.accept() else { return };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head, mut body) = loop {
                    let n = s.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break (String::from_utf8_lossy(&buf).to_string(), Vec::new());
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break (
                            String::from_utf8_lossy(&buf[..i]).to_string(),
                            buf[i + 4..].to_vec(),
                        );
                    }
                };
                let len = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                while body.len() < len {
                    let n = s.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    body.extend_from_slice(&chunk[..n]);
                }
                rec.lock()
                    .unwrap()
                    .push((head, String::from_utf8_lossy(&body).to_string()));
                let _ = s.write_all(resp.as_bytes());
            }
        });
        Fake { port, requests }
    }

    fn json_ok(body: &str) -> String {
        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }
    fn sse(lines: &[&str]) -> String {
        let body: String = lines.iter().map(|l| format!("data: {l}\n\n")).collect();
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{body}"
        )
    }
    fn status(code: u16, body: &str, extra: &str) -> String {
        format!("HTTP/1.1 {code} X\r\nContent-Type: text/plain\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }
    fn cfg(f: &Fake, stream: bool) -> AiConfig {
        AiConfig {
            base_url: format!("http://127.0.0.1:{}/v1", f.port),
            model: "m1".into(),
            stream,
            timeout_secs: 10,
            ..Default::default()
        }
    }
    fn ask(text: &str) -> ChatRequest {
        ChatRequest {
            messages: vec![ChatMessage {
                role: Role::User,
                content: text.into(),
            }],
            ..Default::default()
        }
    }
    fn signed_in(c: AiConfig) -> AiSession {
        let s = AiSession::new(c);
        s.set_token("tok-SECRET-123", None).unwrap();
        s
    }

    #[test]
    fn non_streaming_chat_sends_the_token_and_parses_text_and_usage() {
        let f = serve(vec![json_ok(
            r#"{"choices":[{"message":{"content":"SELECT 1"},"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":3}}"#,
        )]);
        let s = signed_in(AiConfig {
            max_tokens: Some(50),
            ..cfg(&f, false)
        });
        let mut seen = String::new();
        let r = s
            .chat(&ask("hi"), &AtomicBool::new(false), &mut |Event::Delta(
                d,
            )| {
                seen.push_str(&d)
            })
            .unwrap();
        assert_eq!(
            (r.text.as_str(), r.finish_reason.as_deref()),
            ("SELECT 1", Some("stop"))
        );
        assert_eq!(
            r.usage,
            Some(Usage {
                prompt_tokens: 11,
                completion_tokens: 3
            })
        );
        assert_eq!(seen, "SELECT 1");
        let (head, body) = f.requests.lock().unwrap()[0].clone();
        assert!(head.starts_with("POST /v1/chat/completions"), "{head}");
        assert!(
            head.to_ascii_lowercase()
                .contains("authorization: bearer tok-secret-123"),
            "{head}"
        );
        let b: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            (
                b["model"].as_str(),
                b["stream"].as_bool(),
                b["max_tokens"].as_u64()
            ),
            (Some("m1"), Some(false), Some(50))
        );
        assert_eq!(b["messages"][0]["role"], "user");
    }

    #[test]
    fn streaming_chat_delivers_deltas_in_order() {
        let f = serve(vec![sse(&[
            r#"{"choices":[{"delta":{"role":"assistant"}}]}"#,
            r#"{"choices":[{"delta":{"content":"SEL"}}]}"#,
            ": keep-alive",
            r#"{"choices":[{"delta":{"content":"ECT 1"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":2}}"#,
            "[DONE]",
        ])]);
        let s = signed_in(cfg(&f, true));
        let mut parts = Vec::new();
        let r = s
            .chat(&ask("hi"), &AtomicBool::new(false), &mut |Event::Delta(
                d,
            )| {
                parts.push(d)
            })
            .unwrap();
        assert_eq!(parts, vec!["SEL", "ECT 1"]);
        assert_eq!(r.text, "SELECT 1");
        assert_eq!(r.usage.unwrap().completion_tokens, 2);
        assert!(f.requests.lock().unwrap()[0]
            .0
            .to_ascii_lowercase()
            .contains("accept: text/event-stream"));
    }

    #[test]
    fn cancelling_stops_the_stream_and_keeps_what_arrived() {
        let f = serve(vec![sse(&[
            r#"{"choices":[{"delta":{"content":"A"}}]}"#,
            r#"{"choices":[{"delta":{"content":"B"}}]}"#,
            r#"{"choices":[{"delta":{"content":"C"}}]}"#,
            "[DONE]",
        ])]);
        let s = signed_in(cfg(&f, true));
        let cancel = AtomicBool::new(false);
        let r = s
            .chat(&ask("hi"), &cancel, &mut |Event::Delta(_)| {
                cancel.store(true, Ordering::Relaxed)
            })
            .unwrap();
        assert!(r.cancelled);
        assert_eq!(r.text, "A");
    }

    #[test]
    fn a_rejected_token_signs_the_person_out_and_asks_again() {
        let f = serve(vec![status(401, "bad token tok-SECRET-123", "")]);
        let s = signed_in(cfg(&f, false));
        let e = s
            .chat(&ask("hi"), &AtomicBool::new(false), &mut |_| {})
            .unwrap_err();
        assert!(matches!(e, Error::AiAuth(_)), "{e}");
        assert!(!e.to_string().contains("SECRET"));
        assert!(!s.status().signed_in, "the rejected token is dropped");
    }

    #[test]
    fn missing_or_expired_tokens_fail_before_any_request() {
        let f = serve(vec![json_ok("{}")]);
        let s = AiSession::new(cfg(&f, false));
        assert!(
            matches!(s.chat(&ask("x"), &AtomicBool::new(false), &mut |_| {}), Err(Error::AiAuth(m)) if m.contains("not signed in"))
        );
        s.set_token("t", Some(now_ms() + 10_000)).unwrap();
        assert!(
            matches!(s.chat(&ask("x"), &AtomicBool::new(false), &mut |_| {}), Err(Error::AiAuth(m)) if m.contains("expired")),
            "inside the 30 s safety margin"
        );
        assert!(f.requests.lock().unwrap().is_empty(), "no request was sent");
        assert!(!s.status().signed_in);
    }

    #[test]
    fn server_errors_are_reported_with_status_and_retry_hint_and_never_echo_the_token() {
        let f = serve(vec![
            status(
                429,
                "slow down; your key tok-SECRET-123",
                "Retry-After: 7\r\n",
            ),
            status(500, "boom Bearer tok-SECRET-123", ""),
        ]);
        let s = signed_in(cfg(&f, false));
        let e = s
            .chat(&ask("x"), &AtomicBool::new(false), &mut |_| {})
            .unwrap_err()
            .to_string();
        assert!(e.contains("429") && e.contains("retry after 7"), "{e}");
        let e = s
            .chat(&ask("x"), &AtomicBool::new(false), &mut |_| {})
            .unwrap_err()
            .to_string();
        assert!(e.contains("500") && !e.contains("SECRET"), "{e}");
        assert!(s.status().signed_in, "only a 401 drops the token");
    }

    #[test]
    fn an_answer_in_the_wrong_shape_is_an_error_not_an_empty_reply() {
        let f = serve(vec![json_ok(r#"{"unexpected":true}"#)]);
        let s = signed_in(cfg(&f, false));
        let e = s
            .chat(&ask("x"), &AtomicBool::new(false), &mut |_| {})
            .unwrap_err()
            .to_string();
        assert!(e.contains("unexpected answer shape"), "{e}");
    }

    #[test]
    fn custom_auth_header_extra_headers_and_a_raw_token() {
        let f = serve(vec![json_ok(
            r#"{"choices":[{"message":{"content":"ok"}}]}"#,
        )]);
        let mut c = cfg(&f, false);
        c.auth_header = "X-Api-Key".into();
        c.auth_scheme = String::new();
        c.extra_headers.insert("X-Tenant".into(), "corp".into());
        signed_in(c).test().unwrap();
        let head = f.requests.lock().unwrap()[0].0.to_ascii_lowercase();
        assert!(
            head.contains("x-api-key: tok-secret-123")
                && head.contains("x-tenant: corp")
                && !head.contains("authorization:"),
            "{head}"
        );
    }

    #[test]
    fn settings_that_could_leak_the_token_are_refused() {
        let ok = AiConfig {
            base_url: "https://ai.corp/v1".into(),
            model: "m".into(),
            ..Default::default()
        };
        assert!(ok.validate().is_ok());
        let bad = |f: &dyn Fn(&mut AiConfig)| {
            let mut c = ok.clone();
            f(&mut c);
            c.validate().unwrap_err().to_string()
        };
        assert!(bad(&|c| c.base_url = "http://ai.corp/v1".into()).contains("unencrypted"));
        assert!(AiConfig {
            base_url: "http://localhost:8080/v1".into(),
            ..ok.clone()
        }
        .validate()
        .is_ok());
        assert!(bad(&|c| c.base_url = "https://user:pw@ai.corp/v1".into()).contains("user name"));
        assert!(bad(&|c| c.base_url = "ai.corp".into()).contains("https://"));
        assert!(bad(&|c| c.path = "chat".into()).contains("start with /"));
        assert!(bad(&|c| c.model = " ".into()).contains("model"));
        assert!(bad(&|c| c.auth_header = "A\r\nB".into()).contains("not valid"));
        assert!(bad(&|c| {
            c.extra_headers.insert("authorization".into(), "x".into());
        })
        .contains("set by the app"));
        assert!(bad(&|c| c.timeout_secs = 0).contains("timeout"));
        assert!(AiSession::default().set_token("a\nb", None).is_err());
    }

    #[test]
    fn the_mock_provider_needs_no_network_or_sign_in() {
        let s = AiSession::new(AiConfig {
            provider: "mock".into(),
            ..Default::default()
        });
        assert!(s.status().configured);
        let mut parts = Vec::new();
        let r = s
            .chat(
                &ask("list tables"),
                &AtomicBool::new(false),
                &mut |Event::Delta(d)| parts.push(d),
            )
            .unwrap();
        assert_eq!(r.text, "mock reply to: list tables");
        assert_eq!(parts.len(), 5);
    }

    #[test]
    fn redaction_removes_the_token_and_bearer_values() {
        assert_eq!(
            redact_token("key abcd1234 here", "abcd1234"),
            "key *** here"
        );
        assert_eq!(
            redact_token("Authorization: Bearer xyz.123 end", "other"),
            "Authorization: Bearer *** end"
        );
    }
}
