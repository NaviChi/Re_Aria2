# LOKI ARIAFORGE UI ⚡️

<p align="center">
  <strong>A High-Performance, Asynchronous, Multiplexed Dark Web Downloader</strong>
</p>

## Overview
Loki AriaForge is a premium, cross-platform native desktop application designed for extreme download speeds through connection multiplexing. Built with a high-performance Rust backend and wrapped in a beautiful, hardware-accelerated React/Tauri React interface, AriaForge allows users to bypass bandwidth throttling and accelerate large data extractions.

It features a cutting-edge **Cyberpunk Dark Glassmorphism** aesthetic, simulating a tactical hacking terminal. 

## Key Features
* **Auto Swarm Targeter**: Paste a single url, or thousands. The engine will automatically detect if you are targeting a single file or a batch dataset and execute a swarm queue sequence.
* **Rust Multiplexing Engine**: Connects to nodes using asynchronous, multi-threaded `tokio` partitions to download isolated byte-ranges of files simultaneously and seamlessly reassemble them.
* **Tauri Desktop Native Framework**: Built entirely on Tauri v2 to significantly reduce web-view overhead (compared to Electron) and execute the frontend logic in a highly optimized sandbox. 
* **Tor Daemon Integration**: Enforce routing through local SOCKS5 Tor daemons to securely extract files over `.onion` darknet domains securely.
* **Dynamic GUI Metrics**: Real-time throughput calculations, live byte-tracking progress bars, and high-performance React state management.
* **Cross-Platform Automated CI/CD**: Uses GitHub Actions to compile native `.exe`, `.app`/`.dmg`, and `.deb`/`.AppImage` files in the cloud autonomously upon tagging a release.

---

## 🚀 Getting Started

### Prerequisites
Before running Loki AriaForge, ensure you have the following frameworks installed:
- **[Node.js](https://nodejs.org/)** (v18+)
- **[Rust Toolchain](https://www.rust-lang.org/tools/install)**
- **Platform Specific Dependencies:** (e.g. `xcode-select --install` for macOS or `libwebkit2gtk-4.1-dev` for Linux). See the [Tauri Setup Guide](https://tauri.app/v1/guides/getting-started/prerequisites) for more details.

### Installation

1. Clone the repository:
```bash
git clone https://github.com/YOUR_USERNAME/Loki_AriaForge_UI.git
cd Loki_AriaForge_UI
```

2. Install Node dependencies:
```bash
npm install
```

### Running Locally (Development Mode)
To spin up both the Vite React server and compile the rust backend instantly, utilize the Tauri daemon:
```bash
npm run tauri dev
```

### Compiling Executables Locally (Production Mode)
To compile the raw binary natively to your operating system:
```bash
npm run tauri build
```
*(The generated installer packages will be placed in `src-tauri/target/release/bundle/`)*

---

## ⚙️ Automated Github Cloud Releases
Loki AriaForge contains an advanced multi-platform orchestration script (`.github/workflows/release.yml`). 

To trigger the automated build pipeline and generate Windows, Mac, and Linux binaries using Github's cloud:

```bash
git tag v1.0.0
git push origin v1.0.0
```
Check the **Actions** and **Releases** tabs in your Github repository to download the final artifacts.

---

## DISCLAIMER
*Loki AriaForge is designed for security research, data archival intelligence, and legitimate stress-testing of file transfer protocols. Use responsibly and ensure you have authorization to target designated domains.*
