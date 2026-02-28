use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use tauri::{command, AppHandle, Emitter, RunEvent};

mod downloader;

#[derive(Serialize, Deserialize)]
pub struct DownloadArgs {
    url: String,
    path: String,
    connections: usize,
    force_tor: bool,
}

#[derive(Serialize, Deserialize)]
pub struct BatchDownloadArgs {
    files: Vec<downloader::BatchFileEntry>,
    connections: usize,
    force_tor: bool,
}

#[derive(Clone, Serialize)]
pub struct DownloadFailedEvent {
    url: String,
    path: String,
    error: String,
}

#[derive(Clone, Serialize)]
pub struct FileTreeEntry {
    name: String,
    path: String,
    relative: String,
    is_dir: bool,
    size: Option<u64>,
    modified: Option<u64>,
    depth: usize,
    extension: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct FilePreviewResponse {
    kind: String,
    content: String,
    bytes_read: usize,
    truncated: bool,
}

fn normalize_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
}

#[command]
fn list_output_tree(
    root: String,
    max_entries: Option<usize>,
) -> Result<Vec<FileTreeEntry>, String> {
    let root_path = PathBuf::from(root);
    if !root_path.exists() {
        return Ok(Vec::new());
    }

    let root_canonical = root_path
        .canonicalize()
        .map_err(|err| format!("failed to resolve root: {err}"))?;
    let entry_limit = max_entries.unwrap_or(1200).clamp(10, 5000);

    let mut stack: Vec<(PathBuf, usize)> = vec![(root_canonical.clone(), 0)];
    let mut entries: Vec<FileTreeEntry> = Vec::with_capacity(entry_limit);

    while let Some((current_dir, depth)) = stack.pop() {
        let iter = fs::read_dir(&current_dir).map_err(|err| {
            format!(
                "failed to read directory '{}': {err}",
                current_dir.display()
            )
        })?;

        let mut children: Vec<(PathBuf, bool)> = Vec::new();
        for child in iter {
            let child = child.map_err(|err| format!("failed to read directory entry: {err}"))?;
            let path = child.path();
            let is_dir = child.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            children.push((path, is_dir));
        }

        children.sort_by(
            |(a_path, a_is_dir), (b_path, b_is_dir)| match (*a_is_dir, *b_is_dir) {
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                _ => {
                    let a_name = a_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default();
                    let b_name = b_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default();
                    a_name.cmp(b_name)
                }
            },
        );

        for (path, is_dir) in children {
            if entries.len() >= entry_limit {
                break;
            }

            let metadata = fs::metadata(&path).ok();
            let size = if is_dir {
                None
            } else {
                metadata.as_ref().map(|meta| meta.len())
            };
            let modified = metadata
                .as_ref()
                .and_then(|meta| meta.modified().ok())
                .and_then(|stamp| stamp.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs());

            let relative = path
                .strip_prefix(&root_canonical)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();

            entries.push(FileTreeEntry {
                name,
                path: path.to_string_lossy().to_string(),
                relative,
                is_dir,
                size,
                modified,
                depth,
                extension: normalize_extension(&path),
            });

            if is_dir {
                stack.push((path, depth + 1));
            }
        }

        if entries.len() >= entry_limit {
            break;
        }
    }

    Ok(entries)
}

#[command]
fn read_file_preview(
    path: String,
    max_bytes: Option<usize>,
) -> Result<FilePreviewResponse, String> {
    let limit = max_bytes.unwrap_or(8192).clamp(512, 65536);
    let bytes = fs::read(&path).map_err(|err| format!("failed to read file '{}': {err}", path))?;
    let sampled = bytes.len().min(limit);
    let slice = &bytes[..sampled];

    let printable_count = slice
        .iter()
        .filter(|byte| byte.is_ascii_graphic() || matches!(**byte, b' ' | b'\n' | b'\r' | b'\t'))
        .count();
    let binary_like =
        sampled > 0 && printable_count.saturating_mul(100) < sampled.saturating_mul(70);

    let (kind, content) = if binary_like {
        (
            "binary".to_string(),
            format!(
                "Binary-like content detected. Preview omitted after sampling {} bytes.",
                sampled
            ),
        )
    } else {
        (
            "text".to_string(),
            String::from_utf8_lossy(slice).to_string(),
        )
    };

    Ok(FilePreviewResponse {
        kind,
        content,
        bytes_read: sampled,
        truncated: bytes.len() > sampled,
    })
}

#[command]
async fn initiate_download(app: AppHandle, args: DownloadArgs) -> Result<(), String> {
    let control = downloader::activate_download_control()
        .ok_or_else(|| "A download is already active.".to_string())?;

    let DownloadArgs {
        url,
        path,
        connections,
        force_tor,
    } = args;

    app.emit("log", format!("Initiating extraction for: {url}"))
        .ok();

    let app_clone = app.clone();
    let fail_url = url.clone();
    let fail_path = path.clone();

    tokio::spawn(async move {
        let result = downloader::start_download(
            app_clone.clone(),
            url,
            path,
            connections,
            force_tor,
            control,
        )
        .await;

        downloader::clear_download_control();

        if let Err(err) = result {
            let message = err.to_string();
            let _ = app_clone.emit("log", format!("[ERROR] {message}"));
            let _ = app_clone.emit(
                "download_failed",
                DownloadFailedEvent {
                    url: fail_url,
                    path: fail_path,
                    error: message,
                },
            );
        }
    });

    Ok(())
}

#[command]
async fn initiate_batch_download(app: AppHandle, args: BatchDownloadArgs) -> Result<(), String> {
    let control = downloader::activate_download_control()
        .ok_or_else(|| "A download is already active.".to_string())?;

    let BatchDownloadArgs { files, connections, force_tor } = args;
    let file_count = files.len();

    app.emit("log", format!("[*] Batch download: {} files", file_count)).ok();

    let app_clone = app.clone();
    tokio::spawn(async move {
        let result = downloader::start_batch_download(
            app_clone.clone(), files, connections, force_tor, control,
        ).await;

        downloader::clear_download_control();

        if let Err(err) = result {
            let _ = app_clone.emit("log", format!("[ERROR] Batch: {}", err));
        }
    });

    Ok(())
}

#[command]
fn pause_active_download(app: AppHandle) -> Result<bool, String> {
    let paused = downloader::request_pause();
    if paused {
        let _ = app.emit(
            "log",
            "[*] Pause requested for active download.".to_string(),
        );
    }
    Ok(paused)
}

#[command]
fn stop_active_download(app: AppHandle) -> Result<bool, String> {
    let stopped = downloader::request_stop();
    if stopped {
        let _ = app.emit("log", "[*] Stop requested for active download.".to_string());
    }
    Ok(stopped)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    downloader::cleanup_stale_tor_daemons();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            downloader::cleanup_stale_tor_daemons();
            let _ = app
                .handle()
                .emit("log", "[*] Startup Tor cleanup sweep complete.".to_string());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            initiate_download,
            initiate_batch_download,
            pause_active_download,
            stop_active_download,
            list_output_tree,
            read_file_preview
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| match event {
        RunEvent::ExitRequested { .. } | RunEvent::Exit => {
            let _ = downloader::request_stop();
            downloader::cleanup_stale_tor_daemons();
            let _ = app_handle.emit(
                "tor_status",
                downloader::TorStatusEvent {
                    state: "stopped".to_string(),
                    message: "Tor cleanup complete on exit.".to_string(),
                    daemon_count: 0,
                },
            );
        }
        _ => {}
    });
}
