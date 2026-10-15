#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;

fn main() {
    tauri::Builder::default()
        .manage(commands::AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::list_profiles,
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
