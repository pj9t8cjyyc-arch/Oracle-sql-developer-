//! In-app updates. Built only with `--features updater` together with `tauri.updater.conf.json`
//! (signing public key and feed address); other builds report that updates are not enabled.
//! Downloads are verified against the public key in the build, so a feed alone cannot push code.

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::commands::AppState;

type CmdResult<T> = Result<T, String>;

#[derive(Serialize)]
pub struct UpdateStatus {
    /// This build can update itself.
    enabled: bool,
    current_version: String,
    /// The feed in use: the one set here, else the one built in.
    endpoint: Option<String>,
    /// A feed address was set by the person or an administrator (overrides the built-in one).
    custom_endpoint: bool,
    auto_check: bool,
}

#[derive(Serialize)]
pub struct UpdateInfo {
    version: String,
    notes: Option<String>,
    date: Option<String>,
}

fn setting(state: &State<'_, AppState>, key: &str) -> Option<String> {
    state
        .history_for_ai()
        .lock()
        .unwrap()
        .setting_get(key)
        .ok()
        .flatten()
        .filter(|v| !v.is_empty())
}

#[tauri::command]
pub fn update_status(app: AppHandle, state: State<'_, AppState>) -> UpdateStatus {
    let custom = setting(&state, "update.endpoint");
    UpdateStatus {
        enabled: cfg!(feature = "updater"),
        current_version: app.package_info().version.to_string(),
        endpoint: custom.clone().or_else(|| built_in_endpoint(&app)),
        custom_endpoint: custom.is_some(),
        auto_check: setting(&state, "update.auto").as_deref() != Some("0"),
    }
}

/// `endpoint` empty or missing goes back to the built-in feed.
#[tauri::command]
pub fn update_set(
    app: AppHandle,
    state: State<'_, AppState>,
    auto_check: bool,
    endpoint: Option<String>,
) -> CmdResult<UpdateStatus> {
    let ep = match endpoint.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
        Some(e) => ora_core::updates::normalize_endpoint(e).map_err(|e| e.to_string())?,
        None => String::new(),
    };
    {
        let mut h = state.history_for_ai().lock().unwrap();
        h.setting_set("update.auto", if auto_check { "1" } else { "0" })
            .map_err(|e| e.to_string())?;
        h.setting_set("update.endpoint", &ep)
            .map_err(|e| e.to_string())?;
    }
    Ok(update_status(app, state))
}

#[cfg(feature = "updater")]
mod real {
    use super::*;
    use tauri::Emitter;
    use tauri_plugin_updater::UpdaterExt;

    pub fn built_in_endpoint(app: &AppHandle) -> Option<String> {
        app.config()
            .plugins
            .0
            .get("updater")
            .and_then(|u| u.get("endpoints"))
            .and_then(|e| e.get(0))
            .and_then(|e| e.as_str())
            .map(str::to_string)
    }

    fn updater(
        app: &AppHandle,
        state: &State<'_, AppState>,
    ) -> CmdResult<tauri_plugin_updater::Updater> {
        let mut b = app.updater_builder();
        if let Some(ep) = setting(state, "update.endpoint") {
            let ep = ora_core::updates::normalize_endpoint(&ep).map_err(|e| e.to_string())?;
            b = b
                .endpoints(vec![ep.parse().map_err(|e| format!("bad address: {e}"))?])
                .map_err(|e| e.to_string())?;
        }
        b.build().map_err(|e| e.to_string())
    }

    pub async fn check(
        app: AppHandle,
        state: State<'_, AppState>,
    ) -> CmdResult<Option<UpdateInfo>> {
        let u = updater(&app, &state)?;
        let found = u.check().await.map_err(|e| e.to_string())?;
        Ok(found.map(|u| UpdateInfo {
            version: u.version.clone(),
            notes: u.body.clone(),
            date: u.date.map(|d| d.to_string()),
        }))
    }

    /// Downloads, verifies the signature, installs, then restarts. Progress: `update-progress` {downloaded, total}.
    pub async fn install(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
        let u = updater(&app, &state)?;
        let Some(update) = u.check().await.map_err(|e| e.to_string())? else {
            return Err("there is no newer version".into());
        };
        let (mut got, a2) = (0u64, app.clone());
        update
            .download_and_install(
                move |chunk, total| {
                    got += chunk as u64;
                    let _ = a2.emit(
                        "update-progress",
                        serde_json::json!({ "downloaded": got, "total": total }),
                    );
                },
                || {},
            )
            .await
            .map_err(|e| e.to_string())?;
        app.restart()
    }
}

#[cfg(feature = "updater")]
fn built_in_endpoint(app: &AppHandle) -> Option<String> {
    real::built_in_endpoint(app)
}
#[cfg(not(feature = "updater"))]
fn built_in_endpoint(_: &AppHandle) -> Option<String> {
    None
}

#[tauri::command]
pub async fn update_check(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<Option<UpdateInfo>> {
    #[cfg(feature = "updater")]
    {
        real::check(app, state).await
    }
    #[cfg(not(feature = "updater"))]
    {
        let _ = (app, state);
        Err("This build does not include in-app updates. Install a release build.".into())
    }
}

#[tauri::command]
pub async fn update_install(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    #[cfg(feature = "updater")]
    {
        real::install(app, state).await
    }
    #[cfg(not(feature = "updater"))]
    {
        let _ = (app, state);
        Err("This build does not include in-app updates. Install a release build.".into())
    }
}
