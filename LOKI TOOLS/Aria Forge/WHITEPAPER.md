# Aria Forge - Architecture Notes

## Executive Summary
Aria Forge is a Rust-first Tauri application for high-throughput, queue-based acquisition workflows. It combines chunked transfer logic, Tor-aware routing, and a responsive operator console for forensic and threat-intelligence collection tasks.

## System Components
1. Frontend
- Svelte + TypeScript + Tailwind CSS
- Event-driven telemetry and queue orchestration UI

2. Backend
- Rust (`tokio`, `reqwest`, `tauri`)
- Range-based chunking when supported
- Single-stream fallback when range probing fails

3. IPC/Event Model
- UI dispatches commands via `invoke(...)`
- Backend emits `progress`, `speed`, `log`, `complete`, `download_failed`, and `download_interrupted`

## Download Lifecycle
1. Probe target for content length/range support
2. Start Tor daemons when `.onion` routing is required
3. Spawn circuit tasks (or fallback to one stream)
4. Persist chunk completion state where applicable
5. Emit queue and telemetry events in real time
6. Verify SHA-256 on successful completion

## Operational Safety
- Pause and stop controls are supported end-to-end
- Stale Tor daemons are cleaned at startup and exit
- Retry limits and stream timeouts prevent infinite stall loops

## Delivery
GitHub Actions builds and uploads cross-platform bundles to release tags (`v*`).
