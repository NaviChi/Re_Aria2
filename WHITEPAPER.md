# LOKI ARIAFORGE UI - SYSTEM ARCHITECTURE & WHITEPAPER

## Table of Contents
1. **Executive Summary**
2. **System Architecture**
   2.1. Frontend: React, Vite, & TypeScript
   2.2. Backend: Rust & Tauri Framework
   2.3. IPC Bridge
3. **Download Engine Orchestration**
   3.1. Asynchronous Multiplexing
   3.2. Queue Logic & Task Dispatch
   3.3. File Reassembly
4. **Tor Encapsulation**
5. **Cross-Platform Delivery (CI/CD)**
6. **Future Extensibility**

---

## 1. Executive Summary
Traditional file downloaders handle file transfers synchronously, parsing a massive payload through a singular socket pipeline. **Loki AriaForge** disrupts this limitation by utilizing asynchronous chunk-mapping to "swarm" a file server from multiple logical circuits. By slicing incoming payloads and extracting data non-linearly over multiple worker threads via the Rust `tokio` asynchronous runtime, it effectively obliterates bandwidth throttling metrics. 

By operating entirely within the ultra-low-memory overhead **Tauri Environment**, the cross-platform application is completely native to Linux, macOS, and Windows. A specialized **Cyberpunk Glassmorphism** React HUD sits on top of this engine, displaying live byte metrics and dynamic node connections directly to the operator.

---

## 2. System Architecture

### 2.1. Frontend: React, Vite, & TypeScript
Loki AriaForge utilizes Vite to bundle a high-performance React application injected with strict TypeScript typing.
- **Cyberpunk UI Framework**: Uses raw CSS Variable styling to ensure deterministic hex colors, neon glows, and glassmorphism. Using standard CSS rather than Tailwind limits unnecessary parsing size overhead.
- **State Management**: Variables like `queue`, `targetUrls`, and `isRunning` govern the React Virtual DOM entirely client-side. Live byte transfers passed through the IPC bridge are tracked via `Record<number, ProgressEvent>`, forcing micro-re-renders only on individual Circuit Node cards.

### 2.2. Backend: Rust & Tauri Framework
Relying on `Tauri v2`, the web-based Chromium dependencies native to Electron were discarded. Tauri provides a raw hook to operating system web containers (WebKit on macOS / WebView2 on Windows) executing identical HTML outputs inside lightweight binary frames.
- **Memory Footprint**: Reduced significantly relative to generic browser applications.
- **Tokio Runtime**: Operating as the core heart, spinning up `Async` Green threads to handle network requests efficiently.

### 2.3. IPC Bridge (Inter-Process Communication)
The React UI communicates seamlessly with the Rust OS-layer by firing events over the Tauri IPC Command protocol `invoke("initiate_download", { args: { ... } })`. The Backend broadcasts payload streams over `app.emit("progress", e)` allowing the Javascript frontend to catch byte-streams and generate UI updates effortlessly without blocking.

---

## 3. Download Engine Orchestration

### 3.1. Asynchronous Multiplexing
Traditional single-socket connections hit artificial rate-limits imposed by web servers. 
1. **Pre-flight Check**: The Rust backend issues an `HTTP HEAD` or byte `Range: 0-0` request to determine `Content-Length`.
2. **Chunk Division**: The length is divided mathematically by the user's `Multiplex Limit` (default: 150).
3. **Thread Swarm**: 150 unique async background tasks invoke `tokio::spawn`, each utilizing `reqwest` to issue independent `Range` requests against specific byte chunks of the file.

### 3.2. Queue Logic & Task Dispatch
The UI intelligently manages automated sequences.
If a user inputs an array of separate URLs, the `.tsx` state parses the string dynamically by line-breaks (`\n`), instantiates a generic `filename` for each, and maps them to a `queue`.

### 3.3. File Reassembly
The `downloader.rs` handles IO streams by utilizing a mapped temp-directory structure or `.part` segmented files. By writing byte matrices at specific `seek()` offsets matching their calculated start positions, the file is re-assembled seamlessly into a complete, pristine binary. Upon sequence completion, a SHA256 integrity hash is computed to verify exact binary structure integrity.

---

## 4. Tor Encapsulation
An optional `force_tor` daemon overlay exists to proxy all `reqwest` traffic through local `.onion` circuits (typically `socks5h://127.0.0.1:9050`). This prevents DNS leakage and operates specifically by overriding default HTTP `client` architectures in Rust with tor-routed client pools.

---

## 5. Cross-Platform Delivery (CI/CD)
To maintain seamless agile code delivery across multiple architectures, a `release.yml` GitHub Actions instance is used.
- Upon git-tag push (e.g. `v1.0.0`), virtual containers targeting MacOS (`darwin`), Windows (`x86_64`), and Linux (`ubuntu`) simultaneously checkout the repository.
- Each container performs native compilation sequences of the Rust backend.
- Node.js bundles the React Assets.
- Results are automatically injected directly onto a freshly published Github 'Release' page, bypassing local machine architecture dependency requirements entirely.

---

## 6. Future Extensibility
Currently, Loki AriaForge acts natively for HTTP/HTTPS targets and custom SOCKS5 masking operations. Future iterations (`v2.x`) may introduce raw WebSocket interception protocols, deeper BitTorrent integrations, and localized IP spoofing logic for raw TCP/UDP port swarming tasks.
