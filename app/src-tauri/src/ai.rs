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
    start_chat(app, &state, request)
}

fn start_chat(
    app: AppHandle,
    state: &State<'_, AppState>,
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

// ------------------------------------------------------------ Ask: schema-aware questions

#[derive(Serialize)]
pub struct AskStarted {
    id: String,
    context: ora_core::context::Context,
}

fn ask_kb(
    state: &State<'_, AppState>,
    conn: &str,
    owner: &str,
) -> CmdResult<Arc<ora_core::knowledge::Knowledge>> {
    crate::commands::kb_for(state, conn, owner)?.ok_or_else(|| {
        "Build the knowledge base for this schema first (Knowledge → Build).".to_string()
    })
}

/// What would be sent to the model for this request, without sending anything.
#[tauri::command]
pub fn ask_context(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    input: ora_core::context::AskInput,
    policy: ora_core::context::ContextPolicy,
) -> CmdResult<ora_core::context::Context> {
    let kb = ask_kb(&state, &conn, &owner)?;
    Ok(ora_core::context::build_context(
        &kb,
        &input.question,
        input.sql.as_deref(),
        &[],
        &policy,
    ))
}

#[tauri::command]
pub fn ask_start(
    app: AppHandle,
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    input: ora_core::context::AskInput,
    policy: ora_core::context::ContextPolicy,
) -> CmdResult<AskStarted> {
    let kb = ask_kb(&state, &conn, &owner)?;
    let context =
        ora_core::context::build_context(&kb, &input.question, input.sql.as_deref(), &[], &policy);
    let messages = ora_core::context::ask_messages(&context, &input);
    let request = ChatRequest {
        messages,
        ..Default::default()
    };
    let id = start_chat(app, &state, request)?;
    Ok(AskStarted { id, context })
}

/// Checks generated SQL against the schema before the person runs it.
#[tauri::command]
pub fn ask_validate(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    sql: String,
) -> CmdResult<Vec<ora_core::context::SqlIssue>> {
    let kb = ask_kb(&state, &conn, &owner)?;
    Ok(ora_core::context::validate_sql(&kb, &sql))
}

#[tauri::command]
pub fn ask_extract_sql(answer: String) -> Vec<String> {
    ora_core::context::extract_sql_blocks(&answer)
}

// ------------------------------------------------------------ proactive assistance

#[derive(Serialize)]
pub struct Assisted<T> {
    value: Option<T>,
    /// `ok`, `no_kb` (build the knowledge base), `auth` (sign in again), `cancelled`, `skipped` or `error`.
    status: &'static str,
    message: Option<String>,
    /// Number of schema objects and approximate tokens that were sent, for the "what was sent" line.
    sent: Option<(usize, usize)>,
}

impl<T> Assisted<T> {
    fn status(status: &'static str) -> Self {
        Self {
            value: None,
            status,
            message: None,
            sent: None,
        }
    }
}

/// Starts a call under `slot`, cancelling the previous call in that slot (typing on supersedes it).
fn assist_slot(state: &State<'_, AppState>, slot: &str) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    if let Some(old) = state
        .ai_calls
        .lock()
        .unwrap()
        .insert(slot.to_string(), flag.clone())
    {
        old.store(true, Ordering::Relaxed);
    }
    flag
}

struct AssistReq {
    slot: &'static str,
    conn: String,
    owner: String,
    screen: ora_core::assist::Screen,
    trigger: ora_core::assist::Trigger,
    policy: ora_core::context::ContextPolicy,
    model: Option<String>,
}

async fn assist_run<T: Send + 'static>(
    state: &State<'_, AppState>,
    req: AssistReq,
    map: impl FnOnce(&str, &ora_core::assist::Screen) -> Option<T> + Send + 'static,
) -> Assisted<T> {
    let AssistReq {
        slot,
        conn,
        owner,
        screen,
        trigger,
        policy,
        model,
    } = req;
    let (conn, owner) = (conn.as_str(), owner.as_str());
    use ora_core::assist::{assist_messages, focus_words};
    if state.ai.status().config_error.is_some() {
        return Assisted::status("skipped");
    }
    if !state.ai.status().signed_in && state.ai.status().provider != "mock" {
        return Assisted::status("auth");
    }
    let Ok(Some(kb)) = crate::commands::kb_for(state, conn, owner) else {
        return Assisted::status("no_kb");
    };
    // Smaller than an explicit question: speed matters more than breadth here.
    let policy = ora_core::context::ContextPolicy {
        max_tokens: policy.max_tokens.min(2000),
        max_objects: policy.max_objects.min(8),
        ..policy
    };
    let ctx = ora_core::context::build_context(
        &kb,
        &focus_words(&screen),
        screen.statement.as_deref(),
        &[],
        &policy,
    );
    let sent = Some((ctx.objects.len(), ctx.tokens_estimate));
    let messages = assist_messages(&ctx, &screen, trigger, &policy);
    let completing = matches!(trigger, ora_core::assist::Trigger::Complete);
    let request = ChatRequest {
        messages,
        model: model.filter(|m| !m.trim().is_empty()),
        max_tokens: Some(if completing { 96 } else { 700 }),
        temperature: Some(if completing { 0.0 } else { 0.2 }),
    };
    let cancel = assist_slot(state, slot);
    let ai = state.ai.clone();
    let c2 = cancel.clone();
    let r = tauri::async_runtime::spawn_blocking(move || ai.chat(&request, &c2, &mut |_| {})).await;
    match r {
        Ok(Ok(res)) if !res.cancelled && !cancel.load(Ordering::Relaxed) => Assisted {
            value: map(&res.text, &screen),
            status: "ok",
            message: None,
            sent,
        },
        Ok(Ok(_)) => Assisted::status("cancelled"),
        Ok(Err(Error::AiAuth(m))) => Assisted {
            message: Some(m),
            ..Assisted::status("auth")
        },
        Ok(Err(e)) => Assisted {
            message: Some(e.to_string()),
            ..Assisted::status("error")
        },
        Err(e) => Assisted {
            message: Some(e.to_string()),
            ..Assisted::status("error")
        },
    }
}

/// Grey text at the cursor. Superseded by the next keystroke.
#[tauri::command]
pub async fn assist_complete(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    screen: ora_core::assist::Screen,
    policy: ora_core::context::ContextPolicy,
    model: Option<String>,
) -> CmdResult<Assisted<String>> {
    if !ora_core::assist::should_complete(&screen.before, &screen.after) {
        return Ok(Assisted::status("skipped"));
    }
    Ok(assist_run(
        &state,
        AssistReq {
            slot: "assist:complete",
            conn,
            owner,
            screen,
            trigger: ora_core::assist::Trigger::Complete,
            policy,
            model,
        },
        |raw, s| ora_core::assist::clean_completion(raw, &s.before),
    )
    .await)
}

/// Suggestions after an error, a result, or a pause in typing.
#[tauri::command]
pub async fn assist_suggest(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    screen: ora_core::assist::Screen,
    trigger: ora_core::assist::Trigger,
    policy: ora_core::context::ContextPolicy,
    model: Option<String>,
) -> CmdResult<Assisted<Vec<ora_core::assist::Suggestion>>> {
    if matches!(trigger, ora_core::assist::Trigger::Complete) {
        return Err("use assist_complete for completions".into());
    }
    Ok(assist_run(
        &state,
        AssistReq {
            slot: "assist:suggest",
            conn,
            owner,
            screen,
            trigger,
            policy,
            model,
        },
        |raw, _| Some(ora_core::assist::parse_suggestions(raw)),
    )
    .await)
}
