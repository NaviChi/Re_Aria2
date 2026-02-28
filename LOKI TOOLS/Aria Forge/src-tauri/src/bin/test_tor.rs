use anyhow::Result;
use reqwest::{Client, Proxy, StatusCode};
use reqwest::header::RANGE;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use futures::StreamExt;
use tokio::task::JoinSet;

const STREAM_TIMEOUT_SECS: u64 = 7;
const MAX_STALL_RETRIES: usize = 30;
const PIECE_SIZE: u64 = 10_485_760; // 10MB
const TARGET_CIRCUITS: usize = 120;
const TOURNAMENT_POOL: usize = 240;
const TOTAL_SIZE: u64 = 21399361266;

fn build_client(circuit_id: usize) -> Result<Client> {
    let proxy_url = format!("socks5h://u{circuit_id}:p{circuit_id}@127.0.0.1:9051");
    let proxy = Proxy::all(&proxy_url)?;
    Ok(Client::builder()
        .proxy(proxy)
        .pool_max_idle_per_host(0)
        .tcp_nodelay(true)
        .build()?)
}

#[tokio::main]
async fn main() -> Result<()> {
    let url = "http://lockbit6vhrjaqzsdj6pqalyideigxv4xycfeyunpx35znogiwmojnid.onion/secret/5ebb49ccc01e4337b258f53deab3588e-6faad228-bbfb-33ff-be8b-c86f7e5ed518/terracaribbean.com/terracaribbean.com.7z";

    let total_pieces = TOTAL_SIZE.div_ceil(PIECE_SIZE) as usize;
    let total_downloaded = Arc::new(AtomicU64::new(0));
    let total_recoveries = Arc::new(AtomicUsize::new(0));
    let pieces_completed = Arc::new(AtomicUsize::new(0));
    let next_piece = Arc::new(AtomicUsize::new(0));
    let promoted_count = Arc::new(AtomicUsize::new(0));
    let eliminated_count = Arc::new(AtomicUsize::new(0));
    let stealing_circuits = Arc::new(AtomicUsize::new(0));
    let steals_won = Arc::new(AtomicUsize::new(0));
    let piece_flags: Arc<Vec<AtomicBool>> = Arc::new(
        (0..total_pieces).map(|_| AtomicBool::new(false)).collect()
    );

    let test_start = Instant::now();

    println!("[*] TOURNAMENT + WORK QUEUE + STEAL MODE TEST");
    println!("[*] Racing {} circuits for {} slots | {} pieces", TOURNAMENT_POOL, TARGET_CIRCUITS, total_pieces);
    println!("[*] Target: {:.2} GB", TOTAL_SIZE as f64 / 1_073_741_824.0);
    println!();

    // Monitor
    let mon_total = Arc::clone(&total_downloaded);
    let mon_recs = Arc::clone(&total_recoveries);
    let mon_pieces = Arc::clone(&pieces_completed);
    let mon_promoted = Arc::clone(&promoted_count);
    let mon_elim = Arc::clone(&eliminated_count);
    let mon_stealing = Arc::clone(&stealing_circuits);
    let mon_steals = Arc::clone(&steals_won);
    let monitor = tokio::spawn(async move {
        let mut prev_dl = 0u64;
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            let dl = mon_total.load(Ordering::Relaxed);
            let elapsed = test_start.elapsed().as_secs_f64();
            let avg = if elapsed > 0.0 { (dl as f64 / elapsed) / 1_048_576.0 } else { 0.0 };
            let now = ((dl - prev_dl) as f64 / 15.0) / 1_048_576.0;
            prev_dl = dl;
            let pct = (dl as f64 / TOTAL_SIZE as f64) * 100.0;
            let done = mon_pieces.load(Ordering::Relaxed);
            let promo = mon_promoted.load(Ordering::Relaxed).min(TARGET_CIRCUITS);
            let elim = mon_elim.load(Ordering::Relaxed);
            let thieves = mon_stealing.load(Ordering::Relaxed);
            let stolen = mon_steals.load(Ordering::Relaxed);
            println!(
                "[STATUS] {:.1}min | {:.2}% | {:.0} MB | avg {:.1} | now {:.1} MB/s | promoted: {} | eliminated: {} | pieces: {}/{} | stealers: {} | steals won: {} | recoveries: {}",
                elapsed / 60.0, pct, dl as f64 / 1_048_576.0, avg, now, promo, elim, done, total_pieces, thieves, stolen, mon_recs.load(Ordering::Relaxed)
            );
        }
    });

    let mut tasks = JoinSet::new();

    for circuit_id in 0..TOURNAMENT_POOL {
        let client = match build_client(circuit_id) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let task_url = url.to_string();
        let task_total = Arc::clone(&total_downloaded);
        let task_recs = Arc::clone(&total_recoveries);
        let task_pieces = Arc::clone(&pieces_completed);
        let task_next = Arc::clone(&next_piece);
        let task_promoted = Arc::clone(&promoted_count);
        let task_elim = Arc::clone(&eliminated_count);
        let task_stealing_count = Arc::clone(&stealing_circuits);
        let task_steals_won = Arc::clone(&steals_won);
        let task_flags = Arc::clone(&piece_flags);

        tasks.spawn(async move {
            // === TOURNAMENT PROBE ===
            let probe_end = PIECE_SIZE.min(TOTAL_SIZE).saturating_sub(1);
            let probe_ok = async {
                let resp = tokio::time::timeout(
                    Duration::from_secs(30),
                    client.get(&task_url)
                        .header(RANGE, format!("bytes=0-{probe_end}"))
                        .header("Connection", "close")
                        .send()
                ).await;

                match resp {
                    Ok(Ok(r)) if r.status() == StatusCode::PARTIAL_CONTENT || r.status() == StatusCode::OK => {
                        let mut stream = r.bytes_stream();
                        let mut bytes = 0u64;
                        loop {
                            match tokio::time::timeout(Duration::from_secs(STREAM_TIMEOUT_SECS), stream.next()).await {
                                Ok(Some(Ok(chunk))) => {
                                    bytes += chunk.len() as u64;
                                    if bytes >= probe_end { return true; }
                                }
                                _ => return bytes > 0,
                            }
                        }
                    }
                    _ => false,
                }
            }.await;

            if !probe_ok {
                task_elim.fetch_add(1, Ordering::Relaxed);
                return;
            }

            let slot = task_promoted.fetch_add(1, Ordering::Relaxed);
            if slot >= TARGET_CIRCUITS {
                task_elim.fetch_add(1, Ordering::Relaxed);
                return;
            }

            println!("[+] Circuit {} PROMOTED (slot {}/{})", circuit_id, slot + 1, TARGET_CIRCUITS);

            // === WORK QUEUE + STEAL MODE ===
            let mut stalls = 0usize;
            let mut stealing = false;

            loop {
                // Grab next piece — from queue or by stealing
                let piece_idx = if !stealing {
                    let idx = task_next.fetch_add(1, Ordering::Relaxed);
                    if idx >= total_pieces {
                        stealing = true;
                        task_stealing_count.fetch_add(1, Ordering::Relaxed);
                        println!("[*] Circuit {} → STEAL MODE", circuit_id);
                        continue;
                    }
                    if task_flags[idx].load(Ordering::Relaxed) {
                        continue; // Already done
                    }
                    idx
                } else {
                    // Steal mode: find any incomplete piece
                    match (0..total_pieces).find(|&i| !task_flags[i].load(Ordering::Relaxed)) {
                        Some(idx) => idx,
                        None => return, // ALL pieces done!
                    }
                };

                let piece_start = piece_idx as u64 * PIECE_SIZE;
                let piece_end = ((piece_idx as u64 + 1) * PIECE_SIZE - 1).min(TOTAL_SIZE.saturating_sub(1));
                let mut offset = piece_start;

                while offset <= piece_end {
                    // In steal mode, check if original owner finished
                    if stealing && task_flags[piece_idx].load(Ordering::Relaxed) {
                        break;
                    }

                    let resp = match tokio::time::timeout(Duration::from_secs(45),
                        client.get(&task_url)
                            .header(RANGE, format!("bytes={offset}-{piece_end}"))
                            .header("Connection", "close")
                            .send()
                    ).await {
                        Ok(Ok(r)) => r,
                        _ => {
                            stalls += 1;
                            task_recs.fetch_add(1, Ordering::Relaxed);
                            if stalls > MAX_STALL_RETRIES { return; }
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            continue;
                        }
                    };

                    if resp.status() != StatusCode::PARTIAL_CONTENT && resp.status() != StatusCode::OK {
                        stalls += 1;
                        task_recs.fetch_add(1, Ordering::Relaxed);
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        continue;
                    }

                    let mut stream = resp.bytes_stream();
                    let mut progressed = false;

                    loop {
                        // Check if owner won the race
                        if stealing && task_flags[piece_idx].load(Ordering::Relaxed) {
                            drop(stream);
                            break;
                        }

                        match tokio::time::timeout(Duration::from_secs(STREAM_TIMEOUT_SECS), stream.next()).await {
                            Ok(Some(Ok(chunk))) => {
                                if chunk.is_empty() { continue; }
                                progressed = true;
                                stalls = 0;
                                offset += chunk.len() as u64;
                                task_total.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                                if offset > piece_end { break; }
                            }
                            _ => {
                                task_recs.fetch_add(1, Ordering::Relaxed);
                                drop(stream);
                                tokio::time::sleep(Duration::from_millis(500)).await;
                                break;
                            }
                        }
                    }

                    if offset > piece_end { break; }
                    if !progressed {
                        stalls += 1;
                        if stalls > MAX_STALL_RETRIES { return; }
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }

                // Mark piece done — only if we actually finished AND it wasn't already done
                if offset > piece_end && !task_flags[piece_idx].load(Ordering::Relaxed) {
                    task_flags[piece_idx].store(true, Ordering::Relaxed);
                    task_pieces.fetch_add(1, Ordering::Relaxed);
                    if stealing {
                        task_steals_won.fetch_add(1, Ordering::Relaxed);
                        println!("[+] Circuit {} STOLE piece {} from slow circuit!", circuit_id, piece_idx);
                    }
                }
            }
        });
    }

    // 30-minute deadline
    let deadline = tokio::time::sleep(Duration::from_secs(1800));
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            result = tasks.join_next() => {
                if result.is_none() { break; }
            }
            _ = &mut deadline => {
                println!("\n[*] 30-MINUTE DEADLINE.");
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
                break;
            }
        }
    }

    monitor.abort();

    let elapsed = test_start.elapsed().as_secs_f64();
    let dl = total_downloaded.load(Ordering::Relaxed);
    let speed = if elapsed > 0.0 { (dl as f64 / elapsed) / 1_048_576.0 } else { 0.0 };

    println!("\n[=========== TOURNAMENT + STEAL MODE RESULTS ===========]");
    println!("  Duration:       {:.1} minutes", elapsed / 60.0);
    println!("  Promoted:       {} / {} circuits", promoted_count.load(Ordering::Relaxed).min(TARGET_CIRCUITS), TOURNAMENT_POOL);
    println!("  Eliminated:     {}", eliminated_count.load(Ordering::Relaxed));
    println!("  Downloaded:     {:.2} GB ({:.2}%)", dl as f64 / 1_073_741_824.0, (dl as f64 / TOTAL_SIZE as f64) * 100.0);
    println!("  Avg Speed:      {:.2} MB/s", speed);
    println!("  Pieces:         {} / {}", pieces_completed.load(Ordering::Relaxed), total_pieces);
    println!("  Stealers:       {} circuits entered steal mode", stealing_circuits.load(Ordering::Relaxed));
    println!("  Steals Won:     {} pieces stolen from slow circuits", steals_won.load(Ordering::Relaxed));
    println!("  Recoveries:     {}", total_recoveries.load(Ordering::Relaxed));
    println!("[======================================================]");

    Ok(())
}
