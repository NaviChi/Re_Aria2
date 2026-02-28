use anyhow::{anyhow, Result};
use reqwest::header::{ACCEPT_RANGES, CONTENT_RANGE, RANGE};
use reqwest::{Client, Proxy, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub fn get_tor_path(app: &AppHandle) -> Result<PathBuf> {
    let mut path = app.path().resource_dir().map_err(|e| anyhow!("Failed to get resource dir: {e}"))?;
    path.push("bin");

    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    path.push("win_x64/tor/tor.exe");

    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    path.push("mac_x64/tor/tor");

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    path.push("mac_aarch64/tor/tor");

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    path.push("linux_x64/tor/tor");

    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64")
    )))]
    path.push("unsupported_platform");

    if !path.exists() {
        return Err(anyhow!("Tor executable not found at {}", path.display()));
    }

    Ok(path)
}

const TOR_DATA_DIR_PREFIX: &str = "loki_tor_";
const TOR_PID_FILE: &str = "loki_tor.pid";
const STREAM_TIMEOUT_SECS: u64 = 20;
const MAX_STALL_RETRIES: usize = 10;

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct DownloadState {
    pub completed_chunks: Vec<bool>,
    pub num_circuits: usize,
    pub chunk_size: u64,
    pub content_length: u64,
}

pub struct WriteMsg {
    pub filepath: String,
    pub offset: u64,
    pub data: bytes::Bytes,
    pub close_file: bool,
    pub chunk_id: usize,
}

#[derive(Clone, Serialize)]
pub struct ProgressEvent {
    pub id: usize,
    pub downloaded: u64,
    pub total: u64,
    pub main_speed_mbps: f64,
    pub status: String,
}

#[derive(Clone, Serialize)]
pub struct TorStatusEvent {
    pub state: String,
    pub message: String,
    pub daemon_count: usize,
}

#[derive(Clone, Serialize)]
pub struct DownloadCompleteEvent {
    pub url: String,
    pub path: String,
    pub hash: String,
}

#[derive(Clone, Serialize)]
pub struct DownloadInterruptedEvent {
    pub url: String,
    pub path: String,
    pub reason: String,
}

#[derive(Clone)]
pub struct DownloadControl {
    pause_requested: Arc<AtomicBool>,
    stop_requested: Arc<AtomicBool>,
}

impl DownloadControl {
    fn new() -> Self {
        Self {
            pause_requested: Arc::new(AtomicBool::new(false)),
            stop_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    fn interruption_reason(&self) -> Option<&'static str> {
        if self.stop_requested.load(Ordering::Relaxed) {
            Some("Stopped")
        } else if self.pause_requested.load(Ordering::Relaxed) {
            Some("Paused")
        } else {
            None
        }
    }
}

static ACTIVE_CONTROL: OnceLock<Mutex<Option<DownloadControl>>> = OnceLock::new();

fn active_control_slot() -> &'static Mutex<Option<DownloadControl>> {
    ACTIVE_CONTROL.get_or_init(|| Mutex::new(None))
}

pub fn activate_download_control() -> Option<DownloadControl> {
    let mut guard = active_control_slot().lock().ok()?;
    if guard.is_some() {
        return None;
    }

    let control = DownloadControl::new();
    *guard = Some(control.clone());
    Some(control)
}

pub fn clear_download_control() {
    if let Ok(mut guard) = active_control_slot().lock() {
        *guard = None;
    }
}

pub fn request_pause() -> bool {
    let guard = match active_control_slot().lock() {
        Ok(guard) => guard,
        Err(_) => return false,
    };

    if let Some(control) = guard.as_ref() {
        control.pause_requested.store(true, Ordering::Relaxed);
        true
    } else {
        false
    }
}

pub fn request_stop() -> bool {
    let guard = match active_control_slot().lock() {
        Ok(guard) => guard,
        Err(_) => return false,
    };

    if let Some(control) = guard.as_ref() {
        control.stop_requested.store(true, Ordering::Relaxed);
        control.pause_requested.store(false, Ordering::Relaxed);
        true
    } else {
        false
    }
}

struct ManagedTorProcess {
    child: Child,
    pid_file: PathBuf,
    data_dir: PathBuf,
}

pub struct TorProcessGuard {
    procs: Vec<ManagedTorProcess>,
}

impl TorProcessGuard {
    fn new() -> Self {
        Self { procs: Vec::new() }
    }

    fn push(&mut self, child: Child, pid_file: PathBuf, data_dir: PathBuf) {
        self.procs.push(ManagedTorProcess {
            child,
            pid_file,
            data_dir,
        });
    }

    fn shutdown_all(&mut self) {
        for proc in &mut self.procs {
            let _ = proc.child.kill();
            let _ = proc.child.wait();
            let _ = fs::remove_file(&proc.pid_file);
            let _ = fs::remove_dir_all(&proc.data_dir);
        }
        self.procs.clear();
    }
}

impl Drop for TorProcessGuard {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}

#[derive(Debug)]
enum TaskOutcome {
    Completed,
    Interrupted(&'static str),
    Failed(String),
}

struct ProbeResult {
    content_length: u64,
    supports_ranges: bool,
}

fn parse_content_range_total(header_value: &str) -> Option<u64> {
    header_value
        .split('/')
        .next_back()
        .and_then(|value| value.parse::<u64>().ok())
}

fn terminate_pid(pid: u32) {
    let _ = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status();
    let _ = Command::new("kill")
        .arg("-KILL")
        .arg(pid.to_string())
        .status();
}

fn cleanup_tor_data_dir(data_dir: &Path) {
    let pid_file = data_dir.join(TOR_PID_FILE);
    if let Ok(pid_value) = fs::read_to_string(&pid_file) {
        if let Ok(pid) = pid_value.trim().parse::<u32>() {
            terminate_pid(pid);
        }
    }
    let _ = fs::remove_file(pid_file);
    let _ = fs::remove_dir_all(data_dir);
}

pub fn cleanup_stale_tor_daemons() {
    let tmp_root = Path::new("/tmp");
    let entries = match fs::read_dir(tmp_root) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };

        if name.starts_with(TOR_DATA_DIR_PREFIX) {
            cleanup_tor_data_dir(&path);
        }
    }
}

async fn wait_with_interrupt(
    control: &DownloadControl,
    duration: Duration,
) -> Option<&'static str> {
    let mut elapsed = Duration::from_millis(0);
    while elapsed < duration {
        if let Some(reason) = control.interruption_reason() {
            return Some(reason);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        elapsed += Duration::from_millis(250);
    }
    None
}

async fn probe_target(client: &Client, url: &str, app: &AppHandle) -> Result<ProbeResult> {
    let mut content_length = 0u64;
    let mut supports_ranges = false;

    match client.head(url).send().await {
        Ok(resp) => {
            content_length = resp.content_length().unwrap_or(0);
            supports_ranges = resp
                .headers()
                .get(ACCEPT_RANGES)
                .and_then(|value| value.to_str().ok())
                .map(|value| value.to_ascii_lowercase().contains("bytes"))
                .unwrap_or(false);
        }
        Err(err) => {
            let _ = app.emit("log", format!("[!] HEAD probe failed: {err}"));
        }
    }

    if content_length == 0 || !supports_ranges {
        let _ = app.emit(
            "log",
            "[*] HEAD probe insufficient. Attempting GET range probe...".to_string(),
        );

        if let Ok(resp) = client.get(url).header(RANGE, "bytes=0-1").send().await {
            if resp.status() == StatusCode::PARTIAL_CONTENT {
                supports_ranges = true;
            }

            if let Some(value) = resp
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
            {
                if let Some(total) = parse_content_range_total(value) {
                    content_length = total;
                }
            }

            if content_length == 0 {
                content_length = resp.content_length().unwrap_or(0);
            }
        }
    }

    Ok(ProbeResult {
        content_length,
        supports_ranges: supports_ranges && content_length > 0,
    })
}

fn range_download_client(is_onion: bool, daemon_port: usize, circuit_id: usize) -> Result<Client> {
    if is_onion {
        let proxy_url = format!("socks5h://u{circuit_id}:p{circuit_id}@127.0.0.1:{daemon_port}");
        let proxy = Proxy::all(&proxy_url)?;
        Ok(Client::builder()
            .proxy(proxy)
            .pool_max_idle_per_host(0)
            .tcp_nodelay(true)
            .build()?)
    } else {
        Ok(Client::builder()
            .pool_max_idle_per_host(0)
            .tcp_nodelay(true)
            .build()?)
    }
}

fn stream_download_client(is_onion: bool) -> Result<Client> {
    if is_onion {
        let proxy = Proxy::all("socks5h://127.0.0.1:9051")?;
        Ok(Client::builder()
            .proxy(proxy)
            .pool_max_idle_per_host(0)
            .tcp_nodelay(true)
            .build()?)
    } else {
        Ok(Client::builder()
            .pool_max_idle_per_host(0)
            .tcp_nodelay(true)
            .build()?)
    }
}

pub async fn start_download(
    app: AppHandle,
    url: String,
    output_target: String,
    num_circuits: usize,
    force_tor: bool,
    control: DownloadControl,
) -> Result<()> {
    let requested_circuits = num_circuits.max(1);
    let is_onion = url.contains(".onion") || force_tor;
    let state_file_path = format!("{}.loki_state", output_target);
    let mut tor_guard = TorProcessGuard::new();

    let mut daemon_count = 0usize;
    if is_onion {
        daemon_count = std::cmp::max(1, (requested_circuits as f64 / 30.0).ceil() as usize);
        let _ = app.emit(
            "tor_status",
            TorStatusEvent {
                state: "starting".to_string(),
                message: format!("Bootstrapping {daemon_count} Tor daemon(s)..."),
                daemon_count,
            },
        );

        for daemon_index in 0..daemon_count {
            if let Some(reason) = control.interruption_reason() {
                let _ = app.emit(
                    "download_interrupted",
                    DownloadInterruptedEvent {
                        url: url.clone(),
                        path: output_target.clone(),
                        reason: reason.to_string(),
                    },
                );
                return Ok(());
            }

            let port = 9051 + daemon_index;
            let data_dir = PathBuf::from(format!("/tmp/{TOR_DATA_DIR_PREFIX}{port}"));
            cleanup_tor_data_dir(&data_dir);
            fs::create_dir_all(&data_dir)?;

            let tor_path = get_tor_path(&app)?;
            let tor_dir = tor_path.parent().unwrap();
            let mut cmd = Command::new(&tor_path);

            #[cfg(target_os = "linux")]
            cmd.env("LD_LIBRARY_PATH", tor_dir);

            #[cfg(target_os = "macos")]
            cmd.env("DYLD_LIBRARY_PATH", tor_dir);

            let child = cmd
                .arg("--SocksPort")
                .arg(format!("{port} IsolateSOCKSAuth"))
                .arg("--DataDirectory")
                .arg(&data_dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|err| anyhow!("failed to launch tor daemon on port {port}: {err}"))?;

            let pid_file = data_dir.join(TOR_PID_FILE);
            let _ = fs::write(&pid_file, child.id().to_string());
            tor_guard.push(child, pid_file, data_dir);
        }

        let _ = app.emit(
            "tor_status",
            TorStatusEvent {
                state: "consensus".to_string(),
                message: "Waiting for Tor consensus bootstrap...".to_string(),
                daemon_count,
            },
        );

        if let Some(reason) = wait_with_interrupt(&control, Duration::from_secs(20)).await {
            let _ = app.emit(
                "download_interrupted",
                DownloadInterruptedEvent {
                    url: url.clone(),
                    path: output_target.clone(),
                    reason: reason.to_string(),
                },
            );
            return Ok(());
        }

        let _ = app.emit(
            "tor_status",
            TorStatusEvent {
                state: "ready".to_string(),
                message: "Tor circuits ready.".to_string(),
                daemon_count,
            },
        );
    } else {
        let _ = app.emit(
            "tor_status",
            TorStatusEvent {
                state: "clearnet".to_string(),
                message: "Clearnet target detected. Tor bootstrap skipped.".to_string(),
                daemon_count: 0,
            },
        );
    }

    let sniff_client = stream_download_client(is_onion)?;
    let probe = probe_target(&sniff_client, &url, &app).await?;
    let range_mode = probe.supports_ranges;

    let effective_circuits = if range_mode {
        requested_circuits
            .min(probe.content_length.max(1) as usize)
            .max(1)
    } else {
        1
    };

    if !range_mode {
        let _ = app.emit(
            "log",
            "[!] Byte-range support unavailable. Falling back to single-stream mode.".to_string(),
        );
    }

    let mut state = DownloadState {
        completed_chunks: vec![false; effective_circuits],
        num_circuits: effective_circuits,
        chunk_size: if range_mode {
            probe.content_length / effective_circuits as u64
        } else {
            0
        },
        content_length: if range_mode { probe.content_length } else { 0 },
    };

    let mut is_resuming = false;
    if range_mode && Path::new(&state_file_path).exists() {
        if let Ok(content) = fs::read_to_string(&state_file_path) {
            if let Ok(parsed) = serde_json::from_str::<DownloadState>(&content) {
                if parsed.num_circuits == effective_circuits
                    && parsed.content_length == state.content_length
                    && parsed.completed_chunks.len() == effective_circuits
                {
                    state = parsed;
                    is_resuming = true;
                    let done = state.completed_chunks.iter().filter(|done| **done).count();
                    let _ = app.emit(
                        "log",
                        format!("[+] Resuming from saved state ({done}/{effective_circuits} chunks complete)."),
                    );
                }
            }
        }
    }

    if let Some(parent_dir) = Path::new(&output_target).parent() {
        fs::create_dir_all(parent_dir)?;
    }

    if !is_resuming {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&output_target)?;
    }

    if range_mode {
        fs::write(&state_file_path, serde_json::to_string(&state)?)?;
    } else {
        let _ = fs::remove_file(&state_file_path);
    }

    let (tx, mut rx) = mpsc::channel::<WriteMsg>(3000);
    let state_for_writer = if range_mode {
        Some((state.clone(), state_file_path.clone()))
    } else {
        None
    };

    let writer_handle = tokio::task::spawn_blocking(move || -> Result<()> {
        let mut open_files: HashMap<String, File> = HashMap::new();
        let mut local_state = state_for_writer;

        while let Some(msg) = rx.blocking_recv() {
            if !msg.data.is_empty() {
                if !open_files.contains_key(&msg.filepath) {
                    if let Some(dir) = Path::new(&msg.filepath).parent() {
                        fs::create_dir_all(dir)?;
                    }
                    let file = OpenOptions::new()
                        .write(true)
                        .create(true)
                        .open(&msg.filepath)?;
                    open_files.insert(msg.filepath.clone(), file);
                }

                if let Some(file) = open_files.get_mut(&msg.filepath) {
                    file.seek(SeekFrom::Start(msg.offset))?;
                    file.write_all(&msg.data)?;
                }
            }

            if msg.close_file {
                if let Some((state, path)) = local_state.as_mut() {
                    if msg.chunk_id < state.completed_chunks.len() {
                        state.completed_chunks[msg.chunk_id] = true;
                        fs::write(path, serde_json::to_string(state)?)?;
                    }
                }
                open_files.remove(&msg.filepath);
            }
        }

        Ok(())
    });

    let total_downloaded = Arc::new(AtomicU64::new(0));
    let run_flag = Arc::new(AtomicBool::new(true));
    let start_time = Instant::now();

    let watcher_total = Arc::clone(&total_downloaded);
    let watcher_running = Arc::clone(&run_flag);
    let watcher_app = app.clone();
    let speed_handle = tokio::spawn(async move {
        while watcher_running.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let downloaded = watcher_total.load(Ordering::Relaxed);
            let elapsed = start_time.elapsed().as_secs_f64();
            let mbps = if elapsed > 0.0 {
                (downloaded as f64 / elapsed) / 1048576.0
            } else {
                0.0
            };
            let _ = watcher_app.emit("speed", mbps);
        }
        let _ = watcher_app.emit("speed", 0.0f64);
    });

    let mut tasks: Vec<JoinHandle<TaskOutcome>> = Vec::new();

    if range_mode {
        let content_length = state.content_length;
        let chunk_size = (state.chunk_size).max(1);

        for circuit_id in 0..effective_circuits {
            if state.completed_chunks[circuit_id] {
                continue;
            }

            let start_byte = circuit_id as u64 * chunk_size;
            let end_byte = if circuit_id == effective_circuits - 1 {
                content_length.saturating_sub(1)
            } else {
                ((circuit_id as u64 + 1) * chunk_size).saturating_sub(1)
            };

            if start_byte > end_byte {
                continue;
            }

            let daemon_port = 9051 + (circuit_id % daemon_count.max(1));
            let circuit_client = match range_download_client(is_onion, daemon_port, circuit_id) {
                Ok(client) => client,
                Err(err) => {
                    run_flag.store(false, Ordering::Relaxed);
                    drop(tx);
                    let _ = speed_handle.await;
                    let _ = writer_handle.await;
                    return Err(err);
                }
            };

            let task_tx = tx.clone();
            let task_app = app.clone();
            let task_url = url.clone();
            let task_path = output_target.clone();
            let task_control = control.clone();
            let task_running = Arc::clone(&run_flag);
            let task_total = Arc::clone(&total_downloaded);

            tasks.push(tokio::spawn(async move {
                use futures::StreamExt;

                let mut current_offset = start_byte;
                let total_for_circuit = end_byte.saturating_sub(start_byte) + 1;
                let circuit_start = Instant::now();
                let mut stalls = 0usize;

                while current_offset <= end_byte && task_running.load(Ordering::Relaxed) {
                    if let Some(reason) = task_control.interruption_reason() {
                        task_running.store(false, Ordering::Relaxed);
                        return TaskOutcome::Interrupted(reason);
                    }

                    let response = match circuit_client
                        .get(&task_url)
                        .header(RANGE, format!("bytes={current_offset}-{end_byte}"))
                        .header("Connection", "close")
                        .send()
                        .await
                    {
                        Ok(resp) => resp,
                        Err(err) => {
                            stalls += 1;
                            if stalls > MAX_STALL_RETRIES {
                                return TaskOutcome::Failed(format!(
                                    "circuit {} request failed repeatedly: {}",
                                    circuit_id, err
                                ));
                            }
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            continue;
                        }
                    };

                    if response.status() != StatusCode::PARTIAL_CONTENT
                        && response.status() != StatusCode::OK
                    {
                        stalls += 1;
                        if stalls > MAX_STALL_RETRIES {
                            return TaskOutcome::Failed(format!(
                                "circuit {} bad status: {}",
                                circuit_id,
                                response.status()
                            ));
                        }
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        continue;
                    }

                    let mut stream = response.bytes_stream();
                    let mut progressed = false;

                    loop {
                        if let Some(reason) = task_control.interruption_reason() {
                            task_running.store(false, Ordering::Relaxed);
                            return TaskOutcome::Interrupted(reason);
                        }

                        match tokio::time::timeout(
                            Duration::from_secs(STREAM_TIMEOUT_SECS),
                            stream.next(),
                        )
                        .await
                        {
                            Ok(Some(Ok(chunk))) => {
                                if chunk.is_empty() {
                                    continue;
                                }

                                progressed = true;
                                stalls = 0;

                                let len = chunk.len() as u64;
                                if task_tx
                                    .send(WriteMsg {
                                        filepath: task_path.clone(),
                                        offset: current_offset,
                                        data: chunk,
                                        close_file: false,
                                        chunk_id: circuit_id,
                                    })
                                    .await
                                    .is_err()
                                {
                                    return TaskOutcome::Failed(
                                        "writer channel closed unexpectedly".to_string(),
                                    );
                                }

                                current_offset = current_offset.saturating_add(len);
                                task_total.fetch_add(len, Ordering::Relaxed);

                                let downloaded = current_offset
                                    .saturating_sub(start_byte)
                                    .min(total_for_circuit);
                                let elapsed = circuit_start.elapsed().as_secs_f64();
                                let speed = if elapsed > 0.0 {
                                    (downloaded as f64 / elapsed) / 1048576.0
                                } else {
                                    0.0
                                };

                                let _ = task_app.emit(
                                    "progress",
                                    ProgressEvent {
                                        id: circuit_id,
                                        downloaded,
                                        total: total_for_circuit,
                                        main_speed_mbps: speed,
                                        status: "Active".to_string(),
                                    },
                                );

                                if current_offset > end_byte {
                                    break;
                                }
                            }
                            Ok(Some(Err(err))) => {
                                let _ = task_app.emit(
                                    "log",
                                    format!("[!] Circuit {} stream error: {}", circuit_id, err),
                                );
                                break;
                            }
                            Ok(None) => break,
                            Err(_) => {
                                let _ = task_app.emit(
                                    "log",
                                    format!(
                                        "[!] Circuit {} stalled for {}s. Reconnecting...",
                                        circuit_id, STREAM_TIMEOUT_SECS
                                    ),
                                );
                                break;
                            }
                        }
                    }

                    if current_offset > end_byte {
                        if task_tx
                            .send(WriteMsg {
                                filepath: task_path.clone(),
                                offset: 0,
                                data: bytes::Bytes::new(),
                                close_file: true,
                                chunk_id: circuit_id,
                            })
                            .await
                            .is_err()
                        {
                            return TaskOutcome::Failed(
                                "writer channel closed unexpectedly".to_string(),
                            );
                        }

                        let elapsed = circuit_start.elapsed().as_secs_f64();
                        let speed = if elapsed > 0.0 {
                            (total_for_circuit as f64 / elapsed) / 1048576.0
                        } else {
                            0.0
                        };

                        let _ = task_app.emit(
                            "progress",
                            ProgressEvent {
                                id: circuit_id,
                                downloaded: total_for_circuit,
                                total: total_for_circuit,
                                main_speed_mbps: speed,
                                status: "Done".to_string(),
                            },
                        );

                        return TaskOutcome::Completed;
                    }

                    if !progressed {
                        stalls += 1;
                        if stalls > MAX_STALL_RETRIES {
                            return TaskOutcome::Failed(format!(
                                "circuit {} stalled too many times",
                                circuit_id
                            ));
                        }
                    }

                    tokio::time::sleep(Duration::from_secs(2)).await;
                }

                if let Some(reason) = task_control.interruption_reason() {
                    TaskOutcome::Interrupted(reason)
                } else if current_offset > end_byte {
                    TaskOutcome::Completed
                } else {
                    TaskOutcome::Failed(format!("circuit {} stopped before completion", circuit_id))
                }
            }));
        }
    } else {
        let stream_client = stream_download_client(is_onion)?;
        let task_tx = tx.clone();
        let task_app = app.clone();
        let task_url = url.clone();
        let task_path = output_target.clone();
        let task_control = control.clone();
        let task_running = Arc::clone(&run_flag);
        let task_total = Arc::clone(&total_downloaded);
        let total_hint = probe.content_length;

        tasks.push(tokio::spawn(async move {
            use futures::StreamExt;

            let mut current_offset = 0u64;
            let circuit_start = Instant::now();
            let mut retries = 0usize;

            while task_running.load(Ordering::Relaxed) {
                if let Some(reason) = task_control.interruption_reason() {
                    task_running.store(false, Ordering::Relaxed);
                    return TaskOutcome::Interrupted(reason);
                }

                let response = match stream_client
                    .get(&task_url)
                    .header("Connection", "close")
                    .send()
                    .await
                {
                    Ok(resp) => resp,
                    Err(err) => {
                        retries += 1;
                        if retries > MAX_STALL_RETRIES {
                            return TaskOutcome::Failed(format!(
                                "stream request failed repeatedly: {}",
                                err
                            ));
                        }
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        continue;
                    }
                };

                if !response.status().is_success() {
                    retries += 1;
                    if retries > MAX_STALL_RETRIES {
                        return TaskOutcome::Failed(format!(
                            "stream returned non-success status: {}",
                            response.status()
                        ));
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }

                let mut stream = response.bytes_stream();
                let mut progressed = false;

                loop {
                    if let Some(reason) = task_control.interruption_reason() {
                        task_running.store(false, Ordering::Relaxed);
                        return TaskOutcome::Interrupted(reason);
                    }

                    match tokio::time::timeout(
                        Duration::from_secs(STREAM_TIMEOUT_SECS),
                        stream.next(),
                    )
                    .await
                    {
                        Ok(Some(Ok(chunk))) => {
                            if chunk.is_empty() {
                                continue;
                            }

                            progressed = true;
                            retries = 0;

                            let len = chunk.len() as u64;
                            if task_tx
                                .send(WriteMsg {
                                    filepath: task_path.clone(),
                                    offset: current_offset,
                                    data: chunk,
                                    close_file: false,
                                    chunk_id: 0,
                                })
                                .await
                                .is_err()
                            {
                                return TaskOutcome::Failed(
                                    "writer channel closed unexpectedly".to_string(),
                                );
                            }

                            current_offset = current_offset.saturating_add(len);
                            task_total.fetch_add(len, Ordering::Relaxed);

                            let elapsed = circuit_start.elapsed().as_secs_f64();
                            let speed = if elapsed > 0.0 {
                                (current_offset as f64 / elapsed) / 1048576.0
                            } else {
                                0.0
                            };

                            let _ = task_app.emit(
                                "progress",
                                ProgressEvent {
                                    id: 0,
                                    downloaded: current_offset,
                                    total: total_hint.max(current_offset),
                                    main_speed_mbps: speed,
                                    status: "Active".to_string(),
                                },
                            );
                        }
                        Ok(Some(Err(err))) => {
                            let _ = task_app.emit("log", format!("[!] Stream error: {err}"));
                            break;
                        }
                        Ok(None) => {
                            if task_tx
                                .send(WriteMsg {
                                    filepath: task_path.clone(),
                                    offset: 0,
                                    data: bytes::Bytes::new(),
                                    close_file: true,
                                    chunk_id: 0,
                                })
                                .await
                                .is_err()
                            {
                                return TaskOutcome::Failed(
                                    "writer channel closed unexpectedly".to_string(),
                                );
                            }

                            let elapsed = circuit_start.elapsed().as_secs_f64();
                            let speed = if elapsed > 0.0 {
                                (current_offset as f64 / elapsed) / 1048576.0
                            } else {
                                0.0
                            };

                            let _ = task_app.emit(
                                "progress",
                                ProgressEvent {
                                    id: 0,
                                    downloaded: current_offset,
                                    total: total_hint.max(current_offset),
                                    main_speed_mbps: speed,
                                    status: "Done".to_string(),
                                },
                            );

                            return TaskOutcome::Completed;
                        }
                        Err(_) => {
                            let _ = task_app.emit(
                                "log",
                                format!(
                                    "[!] Stream stalled for {}s. Reconnecting...",
                                    STREAM_TIMEOUT_SECS
                                ),
                            );
                            break;
                        }
                    }
                }

                if !progressed {
                    retries += 1;
                    if retries > MAX_STALL_RETRIES {
                        return TaskOutcome::Failed("stream stalled too many times".to_string());
                    }
                }

                tokio::time::sleep(Duration::from_secs(2)).await;
            }

            if let Some(reason) = task_control.interruption_reason() {
                TaskOutcome::Interrupted(reason)
            } else {
                TaskOutcome::Failed("stream stopped before completion".to_string())
            }
        }));
    }

    drop(tx);

    let mut interruption: Option<&'static str> = None;
    let mut failure: Option<String> = None;

    for task in tasks {
        match task.await {
            Ok(TaskOutcome::Completed) => {}
            Ok(TaskOutcome::Interrupted(reason)) => {
                interruption.get_or_insert(reason);
            }
            Ok(TaskOutcome::Failed(err)) => {
                failure.get_or_insert(err);
            }
            Err(err) => {
                failure.get_or_insert(format!("download task join failure: {err}"));
            }
        }
    }

    run_flag.store(false, Ordering::Relaxed);
    let _ = speed_handle.await;

    match writer_handle.await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            failure.get_or_insert(err.to_string());
        }
        Err(err) => {
            failure.get_or_insert(format!("writer task join failure: {err}"));
        }
    }

    let _ = app.emit(
        "tor_status",
        TorStatusEvent {
            state: "stopped".to_string(),
            message: "Tor daemons shutting down.".to_string(),
            daemon_count,
        },
    );

    if let Some(reason) = interruption {
        if reason == "Stopped" {
            let _ = fs::remove_file(&state_file_path);
        }

        let _ = app.emit(
            "log",
            format!(
                "[*] Download {} for {}",
                reason.to_lowercase(),
                output_target
            ),
        );

        let _ = app.emit(
            "download_interrupted",
            DownloadInterruptedEvent {
                url,
                path: output_target,
                reason: reason.to_string(),
            },
        );
        return Ok(());
    }

    if let Some(err) = failure {
        return Err(anyhow!(err));
    }

    let _ = app.emit(
        "log",
        "[+] Download complete. Verifying SHA256...".to_string(),
    );

    let mut file = File::open(&output_target)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let bytes = file.read(&mut buffer)?;
        if bytes == 0 {
            break;
        }
        hasher.update(&buffer[..bytes]);
    }

    let hash = hex::encode(hasher.finalize());
    let _ = app.emit(
        "complete",
        DownloadCompleteEvent {
            url,
            path: output_target,
            hash,
        },
    );

    let _ = fs::remove_file(state_file_path);
    Ok(())
}
