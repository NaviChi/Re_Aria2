use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, command};

mod downloader;

#[derive(Serialize, Deserialize)]
pub struct DownloadArgs {
    url: String,
    path: String,
    connections: usize,
    force_tor: bool,
}

#[derive(Clone, Serialize)]
pub struct DownloadFailedEvent {
    url: String,
    path: String,
    error: String,
}

#[command]
async fn initiate_download(app: AppHandle, args: DownloadArgs) -> Result<(), String> {
    app.emit("log", format!("Initiating extraction for: {}", args.url)).unwrap();
    
    // Spawn in background
    let app_clone = app.clone();
    let target_url = args.url.clone();
    let target_path = args.path.clone();
    tokio::spawn(async move {
        if let Err(e) = downloader::start_download(
            app_clone.clone(),
            args.url,
            args.path,
            args.connections,
            args.force_tor,
        ).await {
            let err = e.to_string();
            let _ = app_clone.emit("log", format!("[ERROR] {}", err));
            let _ = app_clone.emit("download_failed", DownloadFailedEvent {
                url: target_url,
                path: target_path,
                error: err,
            });
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
