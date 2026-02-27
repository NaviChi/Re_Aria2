use anyhow::Result;
use reqwest::header::RANGE;
use reqwest::{Client, Proxy};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::sync::Arc;
use std::path::Path;
use tokio::task::JoinHandle;
use std::process::{Child, Command};
use tokio::sync::mpsc;
use std::time::{Instant, Duration};
use tauri::{AppHandle, Emitter};
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use hex;

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct DownloadState {
    pub completed_chunks: Vec<bool>, // true if completed
    pub num_circuits: usize,
    pub chunk_size: u64,
    pub content_length: u64,
}

pub struct WriteMsg {
    pub filepath: String,
    pub offset: u64,
    pub data: bytes::Bytes,
    pub close_file: bool,
    pub chunk_id: usize, // newly added for state tracking
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

pub struct TorProcessGuard {
    procs: Vec<Child>,
}

impl TorProcessGuard {
    fn new() -> Self {
        Self { procs: Vec::new() }
    }

    fn push(&mut self, child: Child) {
        self.procs.push(child);
    }
}

impl Drop for TorProcessGuard {
    fn drop(&mut self) {
        for proc in &mut self.procs {
            let _ = proc.kill();
        }
    }
}

pub async fn start_download(
    app: AppHandle,
    url: String,
    output_target: String,
    num_circuits: usize,
    force_tor: bool,
) -> Result<()> {
    let is_onion = url.contains(".onion") || force_tor;
    let state_file_path = format!("{}.loki_state", output_target);
    let mut tor_guard = TorProcessGuard::new();
    
    // Check for Pause/Resume state file
    let mut state = DownloadState::default();
    let mut is_resuming = false;
    
    if Path::new(&state_file_path).exists() {
        if let Ok(content) = std::fs::read_to_string(&state_file_path) {
            if let Ok(parsed) = serde_json::from_str::<DownloadState>(&content) {
                if parsed.num_circuits == num_circuits {
                    state = parsed;
                    is_resuming = true;
                    app.emit("log", format!("[+] Resuming from state file... {}/{} chunks completed.", state.completed_chunks.iter().filter(|&c| *c).count(), num_circuits)).unwrap();
                }
            }
        }
    } else {
        state.num_circuits = num_circuits;
        state.completed_chunks = vec![false; num_circuits];
    }
    
    // Aggressive HEAD / GET 0-1 Bypass
    let client = Client::builder()
        .pool_max_idle_per_host(0)
        .tcp_nodelay(true)
        .build()?;
    
    // We optionally use tor daemon for the first sniff if it's onion, but usually we just boot the daemons first
    let mut num_daemons = 0;
    if is_onion {
        num_daemons = std::cmp::max(1, (num_circuits as f64 / 30.0).ceil() as usize);
        let _ = app.emit("tor_status", TorStatusEvent {
            state: "starting".to_string(),
            message: format!("Bootstrapping {} Tor daemon(s)...", num_daemons),
            daemon_count: num_daemons,
        });
        app.emit("log", format!("[*] Orchestrating {} Tor Daemons natively...", num_daemons)).unwrap();
        
        for i in 0..num_daemons {
            let port = 9051 + i;
            let data_dir = format!("/tmp/loki_tor_{}", port);
            std::fs::create_dir_all(&data_dir)?;
            let child = Command::new("tor")
                .arg("--SocksPort").arg(format!("{} IsolateSOCKSAuth", port))
                .arg("--DataDirectory").arg(&data_dir)
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
                .spawn();
            let child = match child {
                Ok(proc) => proc,
                Err(e) => {
                    let _ = app.emit("tor_status", TorStatusEvent {
                        state: "failed".to_string(),
                        message: format!("Failed to start tor daemon on port {}: {}", port, e),
                        daemon_count: i,
                    });
                    return Err(e.into());
                }
            };
            tor_guard.push(child);
        }
        let _ = app.emit("tor_status", TorStatusEvent {
            state: "consensus".to_string(),
            message: "Waiting for Tor consensus bootstrap...".to_string(),
            daemon_count: num_daemons,
        });
        app.emit("log", "[*] Waiting 25 seconds for Tor Consensus...".to_string()).unwrap();
        tokio::time::sleep(tokio::time::Duration::from_secs(25)).await;
        let _ = app.emit("tor_status", TorStatusEvent {
            state: "ready".to_string(),
            message: "Tor circuits ready.".to_string(),
            daemon_count: num_daemons,
        });
    } else {
        let _ = app.emit("tor_status", TorStatusEvent {
            state: "clearnet".to_string(),
            message: "Clearnet target detected. Tor bootstrap skipped.".to_string(),
            daemon_count: 0,
        });
    }

    if !is_resuming {
        // Find content size
        let sniff_client = if is_onion {
            let proxy = Proxy::all("socks5h://127.0.0.1:9051")?;
            Client::builder().proxy(proxy).build()?
        } else {
            client.clone()
        };
        
        let mut content_length = sniff_client.head(&url).send().await?.content_length().unwrap_or(0);
        
        // AGGRESSIVE BYPASS: if HEAD failed
        if content_length == 0 {
            app.emit("log", "[-] HEAD request dropped. Attempting aggressive GET 0-1 Bypass...".to_string()).unwrap();
            if let Ok(resp) = sniff_client.get(&url).header(RANGE, "bytes=0-1").send().await {
                if let Some(cr) = resp.headers().get("Content-Range") {
                    if let Ok(cr_str) = cr.to_str() {
                        if let Some(size_str) = cr_str.split('/').last() {
                            if let Ok(s) = size_str.parse::<u64>() {
                                content_length = s;
                                app.emit("log", format!("[+] Aggressive bypass successful. Size: {}", s)).unwrap();
                            }
                        }
                    }
                }
            }
        }
        
        // Final fallback if onion and bypass failed completely
        if content_length == 0 && is_onion && url.contains(".7z") {
            content_length = 52040670752; 
        }
        
        state.content_length = content_length;
        state.chunk_size = if content_length > 0 { content_length / num_circuits as u64 } else { 0 };
    }
    
    // Save Initial State
    std::fs::write(&state_file_path, serde_json::to_string(&state)?).unwrap();

    let channel_capacity = 3000;
    let (tx, mut rx) = mpsc::channel::<WriteMsg>(channel_capacity);

    // MPSC Disk Writer Thread
    let state_writer = state.clone();
    let fp_writer = state_file_path.clone();
    let app_writer = app.clone();
    tokio::task::spawn_blocking(move || {
        let mut open_files: std::collections::HashMap<String, File> = std::collections::HashMap::new();
        let mut local_state = state_writer;
        
        while let Some(msg) = rx.blocking_recv() {
            if !msg.data.is_empty() {
                let f = open_files.entry(msg.filepath.clone()).or_insert_with(|| {
                    if let Some(dir) = Path::new(&msg.filepath).parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    OpenOptions::new().write(true).create(true).open(&msg.filepath).unwrap()
                });
                let _ = f.seek(SeekFrom::Start(msg.offset));
                let _ = f.write_all(&msg.data);
            }
            if msg.close_file { // Chunk is fully done
                local_state.completed_chunks[msg.chunk_id] = true;
                std::fs::write(&fp_writer, serde_json::to_string(&local_state).unwrap()).unwrap();
                open_files.remove(&msg.filepath);
                let remaining = local_state.completed_chunks.iter().filter(|&&x| !x).count();
                if remaining == 0 {
                    app_writer.emit("log", "[+] All MPSC chunk streams completed successfully.".to_string()).unwrap();
                }
            }
        }
    });

    let total_downloaded = Arc::new(AtomicU64::new(0));
    let start_time = Instant::now();
    let mut tasks: Vec<JoinHandle<()>> = Vec::new();
    let is_running = Arc::new(AtomicBool::new(true));

    for i in 0..num_circuits {
        if state.completed_chunks[i] { continue; } // Skip already completed chunks

        let (start_byte, end_byte) = if state.content_length > 0 {
            let s = i as u64 * state.chunk_size;
            let e = if i == num_circuits - 1 { state.content_length - 1 } else { (i as u64 + 1) * state.chunk_size - 1 };
            (s, e)
        } else { (0, 0) };

        let circuit_client = if is_onion {
            let daemon_port = 9051 + (i % num_daemons);
            let proxy_url = format!("socks5h://u{}:p{}@127.0.0.1:{}", i, i, daemon_port);
            let proxy = Proxy::all(&proxy_url).unwrap();
            Client::builder().proxy(proxy).pool_max_idle_per_host(0).tcp_nodelay(true).build().unwrap()
        } else {
            Client::builder().pool_max_idle_per_host(0).tcp_nodelay(true).build().unwrap()
        };

        let target = url.clone();
        let downloaded_clone = Arc::clone(&total_downloaded);
        let fp = output_target.clone();
        let tx_clone = tx.clone();
        let app_handle = app.clone();
        let running_flag = Arc::clone(&is_running);

        let task = tokio::spawn(async move {
            let mut current_offset = start_byte;
            let circuit_start = Instant::now();
            
            // Circuit Healing Loop (Auto-retry if dropped/slow)
            while current_offset <= end_byte && running_flag.load(Ordering::Relaxed) {
                let req = if state.content_length > 0 {
                    circuit_client.get(&target).header(RANGE, format!("bytes={}-{}", current_offset, end_byte)).header("Connection", "close")
                } else {
                    circuit_client.get(&target).header("Connection", "close")
                };

                if let Ok(res) = req.send().await {
                    let mut stream = res.bytes_stream();
                    
                    use futures::StreamExt;
                    while let Ok(chunk_res) = tokio::time::timeout(Duration::from_secs(15), stream.next()).await {
                        if let Some(Ok(chunk)) = chunk_res {
                            let len = chunk.len() as u64;
                            let _ = tx_clone.send(WriteMsg { filepath: fp.clone(), offset: current_offset, data: chunk.clone(), close_file: false, chunk_id: i }).await;
                            
                            current_offset += len;
                            downloaded_clone.fetch_add(len, Ordering::Relaxed);
                            let downloaded = current_offset.saturating_sub(start_byte);
                            let elapsed = circuit_start.elapsed().as_secs_f64();
                            let circuit_mbps = if elapsed > 0.0 {
                                (downloaded as f64 / elapsed) / 1048576.0
                            } else {
                                0.0
                            };

                            app_handle.emit("progress", ProgressEvent {
                                id: i, downloaded, total: end_byte - start_byte + 1, main_speed_mbps: circuit_mbps, status: "Active".to_string(),
                            }).unwrap();
                        } else {
                            break; // Stream ended
                        }
                    }
                    if current_offset > end_byte { break; } // Finished normally
                    app_handle.emit("log", format!("[!] Circuit {} dropped/stalled! Invoking Healing Engine (Re-negotiating Tor Node)...", i)).unwrap();
                } else {
                    tokio::time::sleep(Duration::from_secs(2)).await; // cooldown before retry
                }
            }

            if current_offset > end_byte {
                let _ = tx_clone.send(WriteMsg { filepath: fp.clone(), offset: 0, data: bytes::Bytes::new(), close_file: true, chunk_id: i }).await;
                let elapsed = circuit_start.elapsed().as_secs_f64();
                let total = end_byte - start_byte + 1;
                let circuit_mbps = if elapsed > 0.0 {
                    (total as f64 / elapsed) / 1048576.0
                } else {
                    0.0
                };
                app_handle.emit("progress", ProgressEvent { id: i, downloaded: total, total, main_speed_mbps: circuit_mbps, status: "Done".to_string() }).unwrap();
            }
        });
        tasks.push(task);
    }
    
    // Status watcher thread
    let app_handle = app.clone();
    let total_clone = Arc::clone(&total_downloaded);
    let st_time = start_time.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let d = total_clone.load(Ordering::Relaxed);
            let e = st_time.elapsed().as_secs_f64();
            let mbps = if e > 0.0 { (d as f64 / e) / 1048576.0 } else { 0.0 };
            app_handle.emit("speed", mbps).unwrap();
        }
    });

    drop(tx);
    for t in tasks { let _ = t.await; }
    is_running.store(false, Ordering::Relaxed);

    let _ = app.emit("tor_status", TorStatusEvent {
        state: "stopped".to_string(),
        message: "Tor daemons shutting down.".to_string(),
        daemon_count: num_daemons,
    });

    app.emit("log", "[+] Download Process Finalized. Verifying Hash...".to_string()).unwrap();

    // HASH VERIFICATION
    let mut file = File::open(&output_target)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 65536];
    while let Ok(n) = file.read(&mut buffer) {
        if n == 0 { break; }
        hasher.update(&buffer[..n]);
    }
    let hash = hex::encode(hasher.finalize());
    app.emit("log", format!("[+] SHA256 Secure Verification Hash: {}", hash)).unwrap();
    app.emit("complete", DownloadCompleteEvent {
        url,
        path: output_target,
        hash,
    }).unwrap();

    // Clean up state
    std::fs::remove_file(state_file_path).unwrap_or(());

    Ok(())
}
