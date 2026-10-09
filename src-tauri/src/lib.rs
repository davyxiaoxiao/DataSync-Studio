mod commands;
mod data;
mod db;
mod dialect;
mod error;
mod excel;
mod excel_export;
mod jobs;
mod queries;
mod schema;
mod state;
mod store;

#[cfg(test)]
mod tests;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("datasync-studio"));
            let store = store::Store::load(&dir);
            if let Some(warning) = store.storage_warning() {
                eprintln!("[datasync-studio] {warning}");
            }
            app.manage(state::AppState::new(store));
            Ok(())
        })
        .invoke_handler(commands::handlers())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
