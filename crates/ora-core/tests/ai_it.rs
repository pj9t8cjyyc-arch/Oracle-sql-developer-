//! Smoke test against a real AI endpoint. Ignored by default:
//!
//!   AI_BASE_URL=https://ai.your-company.com/v1 AI_MODEL=... AI_TOKEN=... \
//!   [AI_PATH=/chat/completions] [AI_AUTH_HEADER=Authorization] [AI_AUTH_SCHEME=Bearer] \
//!   cargo test -p ora-core --test ai_it -- --ignored --nocapture
//!
//! Checks the endpoint, the model name and the token together, once streaming and once not.

use std::sync::atomic::AtomicBool;

use ora_core::ai::{AiConfig, AiSession, ChatMessage, ChatRequest, Event, Role};

fn env(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_string())
}

fn session(stream: bool) -> AiSession {
    let s = AiSession::new(AiConfig {
        base_url: env("AI_BASE_URL", ""),
        path: env("AI_PATH", "/chat/completions"),
        model: env("AI_MODEL", ""),
        auth_header: env("AI_AUTH_HEADER", "Authorization"),
        auth_scheme: env("AI_AUTH_SCHEME", "Bearer"),
        stream,
        ..Default::default()
    });
    s.set_token(&env("AI_TOKEN", ""), None)
        .expect("set AI_TOKEN");
    s
}

#[test]
#[ignore]
fn the_real_endpoint_answers_streaming_and_not() {
    for stream in [false, true] {
        let s = session(stream);
        let mut deltas = 0;
        let r = s
            .chat(
                &ChatRequest {
                    messages: vec![ChatMessage {
                        role: Role::User,
                        content: "Reply with the single word OK.".into(),
                    }],
                    max_tokens: Some(16),
                    ..Default::default()
                },
                &AtomicBool::new(false),
                &mut |Event::Delta(_)| deltas += 1,
            )
            .unwrap_or_else(|e| panic!("stream={stream}: {e}"));
        println!(
            "stream={stream}: {:?} in {} ms, {deltas} chunk(s), usage {:?}",
            r.text.trim(),
            r.elapsed_ms,
            r.usage
        );
        assert!(!r.text.trim().is_empty());
        assert!(deltas >= 1);
    }
}
