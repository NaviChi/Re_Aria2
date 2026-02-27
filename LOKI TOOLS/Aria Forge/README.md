# Aria Forge

Aria Forge is a Tauri v2 desktop download orchestrator with a Rust backend, Svelte frontend, Tailwind styling, Tor-aware routing, queue controls, and artifact preview tooling.

## Path Layout
- GitHub repo root contains workflow and this tool under: `LOKI TOOLS/Aria Forge`
- Local working path target: `Projects/LOKI TOOLS/Aria Forge`

## Core Features
- Multi-connection chunked downloader with resumable state
- Tor bootstrap and cleanup lifecycle handling
- Queue launch, pause, resume, and stop controls
- Circuit summary metrics (running/avg/min/max speed)
- Artifact tree + preview panel

## Development
Run all commands from `LOKI TOOLS/Aria Forge`:

```bash
npm install
npm run tauri dev
```

## Production Build

```bash
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
npm run tauri build
```

Build outputs are under `src-tauri/target/release/bundle/`.

## Release Automation
Tagging `v*` triggers `.github/workflows/release.yml` to produce cross-platform release artifacts and attach them to the matching GitHub release tag.

## Disclaimer
Use only for authorized security research, forensic collection, and legitimate archival operations.
