use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, command};

mod downloader;

#[derive(Serialize, Deserialize)]
pub struct DownloadArgs {
    url: String,
    path: String,
    connections: usize,
    force_tor: bool,
}

#[command]
async fn initiate_download(app: AppHandle, args: DownloadArgs) -> Result<(), String> {
    app.emit("log", format!("Initiating extraction for: {}", args.url)).unwrap();
    
    // Spawn in background
    let app_clone = app.clone();
    tokio::spawn(async move {
        if let Err(e) = downloader::start_download(
            app_clone.clone(),
            args.url,
            args.path,
            args.connections,
            args.force_tor,
        ).await {
            app_clone.emit("log", format!("Error: {}", e)).unwrap();
        }
    });
    
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![initiate_download])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
