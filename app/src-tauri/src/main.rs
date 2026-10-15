#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod jobs;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            use tauri::Manager;
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            // History and worksheets live in one local SQLite file. If it cannot be
            // opened (read-only disk, newer schema), fall back to memory and say so
            // in the UI instead of failing to start.
            let (store, persistent) =
                match ora_core::history::HistoryStore::open(&dir.join("workspace.sqlite")) {
                    Ok(s) => (s, true),
                    Err(e) => {
                        eprintln!("history: {e}; using an in-memory store");
                        (ora_core::history::HistoryStore::in_memory()?, false)
                    }
                };
            app.manage(commands::AppState::new(store, persistent));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_profiles,
            commands::package_analysis,
            commands::graph_info,
            commands::graph_search,
            commands::graph_focus,
            commands::kb_build,
            commands::kb_status,
            commands::kb_preview,
            commands::kb_export,
            commands::save_profile,
            commands::delete_profile,
            commands::connect,
            commands::disconnect,
            commands::run_query,
            commands::fetch_more,
            commands::close_query,
            commands::cancel_query,
            commands::commit,
            commands::rollback,
            commands::list_schemas,
            commands::object_counts,
            commands::list_objects,
            commands::object_source,
            commands::object_ddl,
            commands::sequence_info,
            commands::object_dependencies,
            commands::statements,
            jobs::job_cancel,
            jobs::export_recommend,
            jobs::export_start,
            jobs::import_profile,
            jobs::import_plan,
            jobs::import_ddl,
            jobs::import_create_table,
            jobs::import_validate_start,
            jobs::import_start,
            commands::history_status,
            commands::history_set_enabled,
            commands::history_search,
            commands::history_suggest,
            commands::history_pin,
            commands::history_forget,
            commands::history_purge,
            commands::ws_save,
            commands::ws_close,
            commands::ws_pin,
            commands::ws_delete,
            commands::ws_list,
            commands::ws_get,
            commands::ws_steps,
            commands::preview_table,
            commands::refresh_metadata,
            commands::table_insight,
            commands::complete,
            commands::references,
            commands::search_objects,
            commands::preview_comment,
            commands::save_comment,
        ])
        .run(tauri::generate_context!())
        .expect("error while running application");
}
