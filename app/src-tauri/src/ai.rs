//! AI commands. The sign-in token arrives from the UI's enterprise login, lives in memory only,
//! and every model call is made here, so tokens and database context never pass through scripts.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ora_core::ai::{AiConfig, AiStatus, ChatRequest, ChatResult, Event};
use ora_core::Error;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::commands::AppState;

type CmdResult<T> = Result<T, String>;

fn history<T>(
    state: &State<'_, AppState>,
    f: impl FnOnce(&mut ora_core::history::HistoryStore) -> ora_core::Result<T>,
) -> CmdResult<T> {
    f(&mut state.history_for_ai().lock().unwrap()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn ai_status(state: State<'_, AppState>) -> AiStatus {
    state.ai.status()
}

#[tauri::command]
pub fn ai_get_config(state: State<'_, AppState>) -> AiConfig {
    state.ai.config()
}

#[tauri::command]
pub fn ai_save_config(state: State<'_, AppState>, config: AiConfig) -> CmdResult<AiStatus> {
    state
        .ai
        .set_config(config.clone())
        .map_err(|e| e.to_string())?;
    let json = serde_json::to_string(&config).map_err(|e| e.to_string())?;
    history(&state, |h| h.setting_set("ai.config", &json))?;
    Ok(state.ai.status())
}

/// Called by the UI right after the enterprise sign-in. `expires_at_ms` is a Unix time in milliseconds.
#[tauri::command]
pub fn ai_set_token(
    state: State<'_, AppState>,
    token: String,
    expires_at_ms: Option<i64>,
) -> CmdResult<AiStatus> {
    state
        .ai
        .set_token(&token, expires_at_ms)
        .map_err(|e| e.to_string())?;
    Ok(state.ai.status())
}

#[tauri::command]
pub fn ai_clear_token(state: State<'_, AppState>) -> AiStatus {
    state.ai.clear_token();
    state.ai.status()
}

#[derive(Serialize)]
pub struct AiTest {
    ok: bool,
    text: String,
    elapsed_ms: u64,
    error: Option<String>,
    /// The person must sign in again (no token, expired, or rejected).
    auth_required: bool,
}

#[tauri::command]
pub async fn ai_test(state: State<'_, AppState>) -> CmdResult<AiTest> {
    let ai = state.ai.clone();
    let r = tauri::async_runtime::spawn_blocking(move || ai.test())
        .await
        .map_err(|e| e.to_string())?;
    Ok(match r {
        Ok(c) => AiTest {
            ok: true,
            text: c.text.trim().chars().take(80).collect(),
            elapsed_ms: c.elapsed_ms,
            error: None,
            auth_required: false,
        },
        Err(e) => AiTest {
            ok: false,
            text: String::new(),
            elapsed_ms: 0,
            auth_required: matches!(e, Error::AiAuth(_)),
            error: Some(e.to_string()),
        },
    })
}

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AiEvent {
    Delta {
        id: String,
        text: String,
    },
    Done {
        id: String,
        result: ChatResult,
    },
    Error {
        id: String,
        message: String,
        auth_required: bool,
    },
}

/// Starts a chat on a background thread; the answer arrives as `ai-event` events.
#[tauri::command]
pub fn ai_chat_start(
    app: AppHandle,
    state: State<'_, AppState>,
    request: ChatRequest,
) -> CmdResult<String> {
    state.ai.status().config_error.map_or(Ok(()), Err)?;
    let id = format!("ai{}", state.job_seq.fetch_add(1, Ordering::Relaxed) + 1);
    let cancel = Arc::new(AtomicBool::new(false));
    state
        .ai_calls
        .lock()
        .unwrap()
        .insert(id.clone(), cancel.clone());
    let ai = state.ai.clone();
    let thread_id = id.clone();
    std::thread::Builder::new()
        .name("ora-ai".into())
        .spawn(move || {
            let r = ai.chat(&request, &cancel, &mut |Event::Delta(text)| {
                let _ = app.emit(
                    "ai-event",
                    AiEvent::Delta {
                        id: thread_id.clone(),
                        text,
                    },
                );
            });
            let ev = match r {
                Ok(result) => AiEvent::Done {
                    id: thread_id.clone(),
                    result,
                },
                Err(e) => AiEvent::Error {
                    id: thread_id.clone(),
                    auth_required: matches!(e, Error::AiAuth(_)),
                    message: e.to_string(),
                },
            };
            app.state::<AppState>()
                .ai_calls
                .lock()
                .unwrap()
                .remove(&thread_id);
            let _ = app.emit("ai-event", ev);
        })
        .map_err(|e| e.to_string())?;
    Ok(id)
}

#[tauri::command]
pub fn ai_cancel(state: State<'_, AppState>, id: String) {
    if let Some(c) = state.ai_calls.lock().unwrap().get(&id) {
        c.store(true, Ordering::Relaxed);
    }
}
