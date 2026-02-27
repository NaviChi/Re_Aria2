<script lang="ts">
  import { onMount } from "svelte";
  import { invoke, convertFileSrc } from "@tauri-apps/api/core";
  import { open } from "@tauri-apps/plugin-dialog";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";

  type QueueStatus = "Pending" | "Active" | "Complete" | "Failed";

  interface ProgressEvent {
    id: number;
    downloaded: number;
    total: number;
    main_speed_mbps: number;
    status: string;
  }

  interface QueuedItem {
    url: string;
    path: string;
    status: QueueStatus;
  }

  interface CompleteEvent {
    url: string;
    path: string;
    hash: string;
  }

  interface DownloadFailedEvent {
    url: string;
    path: string;
    error: string;
  }

  interface TorStatusEvent {
    state: string;
    message: string;
    daemon_count: number;
  }

  interface FileTreeEntry {
    name: string;
    path: string;
    relative: string;
    is_dir: boolean;
    size: number | null;
    modified: number | null;
    depth: number;
    extension: string | null;
  }

  interface FilePreview {
    kind: string;
    content: string;
    bytes_read: number;
    truncated: boolean;
  }

  const imageExtensions = new Set(["png", "jpg", "jpeg", "webp", "gif", "bmp", "ico", "svg", "avif"]);

  let targetUrls = "";
  let outputDir = "/tmp/loki_out/";
  let connections = 120;
  let forceTor = false;
  let isRunning = false;
  let queueDispatchLock = false;
  let speedMbps = 0;
  let logs: string[] = [];
  let circuits: Record<number, ProgressEvent> = {};
  let queue: QueuedItem[] = [];
  let activeQueueIndex: number | null = null;

  let torStatus: TorStatusEvent = {
    state: "idle",
    message: "Awaiting mission parameters.",
    daemon_count: 0,
  };

  let outputEntries: FileTreeEntry[] = [];
  let treeLoading = false;
  let treeError = "";
  let selectedEntry: FileTreeEntry | null = null;
  let previewLoading = false;
  let previewError = "";
  let preview: FilePreview | null = null;

  let logsContainer: HTMLDivElement | null = null;

  $: circuitList = Object.values(circuits).sort((a, b) => a.id - b.id);
  $: totalBytes = circuitList.reduce((sum, entry) => sum + entry.downloaded, 0);
  $: totalMb = (totalBytes / 1048576).toFixed(2);
  $: activeCircuits = circuitList.filter((entry) => entry.status !== "Done").length;
  $: completedQueue = queue.filter((entry) => entry.status === "Complete").length;
  $: failedQueue = queue.filter((entry) => entry.status === "Failed").length;
  $: queueProgress = queue.length > 0 ? Math.round(((completedQueue + failedQueue) / queue.length) * 100) : 0;
  $: selectedImageSrc =
    selectedEntry && !selectedEntry.is_dir && isImageFile(selectedEntry) ? convertFileSrc(selectedEntry.path) : "";

  $: if (logsContainer) {
    logsContainer.scrollTop = logsContainer.scrollHeight;
  }

  function normalizeOutputDirectory(path: string): string {
    if (!path.trim()) {
      return "/tmp/loki_out/";
    }
    return path.endsWith("/") || path.endsWith("\\") ? path : `${path}/`;
  }

  function addLog(message: string): void {
    const stamp = new Date().toISOString().slice(11, 19);
    logs = [...logs.slice(-399), `[${stamp}] ${message}`];
  }

  function formatBytes(value: number | null): string {
    if (value === null || Number.isNaN(value)) {
      return "-";
    }
    if (value < 1024) {
      return `${value} B`;
    }
    if (value < 1024 * 1024) {
      return `${(value / 1024).toFixed(1)} KB`;
    }
    if (value < 1024 * 1024 * 1024) {
      return `${(value / (1024 * 1024)).toFixed(1)} MB`;
    }
    return `${(value / (1024 * 1024 * 1024)).toFixed(2)} GB`;
  }

  function formatTimestamp(value: number | null): string {
    if (!value) {
      return "-";
    }
    return new Date(value * 1000).toLocaleString();
  }

  function queueBadge(status: QueueStatus): string {
    if (status === "Complete") {
      return "bg-emerald-500/15 text-emerald-300 ring-emerald-400/40";
    }
    if (status === "Active") {
      return "bg-cyan-500/15 text-cyan-300 ring-cyan-400/40 pulse-line";
    }
    if (status === "Failed") {
      return "bg-rose-500/15 text-rose-300 ring-rose-400/40";
    }
    return "bg-slate-500/15 text-slate-300 ring-slate-400/40";
  }

  function torBadge(state: string): string {
    if (state === "ready") {
      return "bg-emerald-500/20 text-emerald-300 ring-emerald-400/40";
    }
    if (state === "starting" || state === "consensus") {
      return "bg-amber-500/20 text-amber-300 ring-amber-400/40";
    }
    if (state === "failed") {
      return "bg-rose-500/20 text-rose-300 ring-rose-400/40";
    }
    if (state === "active") {
      return "bg-cyan-500/20 text-cyan-300 ring-cyan-400/40";
    }
    return "bg-slate-500/20 text-slate-300 ring-slate-400/40";
  }

  function deriveFilename(urlValue: string, index: number): string {
    try {
      const parsed = new URL(urlValue);
      const fromPath = parsed.pathname.split("/").filter(Boolean).at(-1);
      if (fromPath) {
        return decodeURIComponent(fromPath).replace(/[\\/:*?"<>|]/g, "_");
      }
    } catch {
      // keep fallback path logic
    }
    const fallback = urlValue.split("/").filter(Boolean).at(-1);
    if (fallback && fallback.length > 0) {
      return fallback.replace(/[\\/:*?"<>|]/g, "_");
    }
    return `target_${index + 1}.bin`;
  }

  function isImageFile(entry: FileTreeEntry): boolean {
    return !!entry.extension && imageExtensions.has(entry.extension.toLowerCase());
  }

  function classifyEntry(entry: FileTreeEntry): string {
    if (entry.is_dir) {
      return "[DIR]";
    }
    if (isImageFile(entry)) {
      return "[IMG]";
    }
    if (entry.extension && ["txt", "md", "json", "yaml", "log", "csv", "toml"].includes(entry.extension.toLowerCase())) {
      return "[TXT]";
    }
    return "[BIN]";
  }

  async function browseDirectory(): Promise<void> {
    const selected = await open({
      directory: true,
      multiple: false,
      title: "Select output directory",
    });
    if (selected && typeof selected === "string") {
      outputDir = normalizeOutputDirectory(selected);
      await refreshArtifacts();
    }
  }

  function setTorIntent(urls: string[]): void {
    const onionIntent = forceTor || urls.some((urlValue) => urlValue.includes(".onion"));
    torStatus = onionIntent
      ? {
          state: "starting",
          message: "Queued Tor-enabled operation. Awaiting daemon bootstrap.",
          daemon_count: 0,
        }
      : {
          state: "clearnet",
          message: "Clearnet queue armed. Tor remains disabled unless forced.",
          daemon_count: 0,
        };
  }

  async function engageQueue(): Promise<void> {
    if (isRunning) {
      return;
    }

    const urls = targetUrls
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter((line) => line.length > 0);

    if (urls.length === 0) {
      addLog("[-] No targets provided.");
      return;
    }

    outputDir = normalizeOutputDirectory(outputDir);
    setTorIntent(urls);
    queue = urls.map((urlValue, index) => {
      const filename = deriveFilename(urlValue, index);
      return {
        url: urlValue,
        path: `${outputDir}${filename}`,
        status: "Pending",
      };
    });

    circuits = {};
    activeQueueIndex = null;
    speedMbps = 0;
    addLog(`[*] Queue staged: ${queue.length} target(s).`);
    await refreshArtifacts();
    void startNextPending();
  }

  function markQueueStatus(index: number, status: QueueStatus): void {
    queue = queue.map((entry, i) => (i === index ? { ...entry, status } : entry));
  }

  function markByPath(path: string, status: QueueStatus): void {
    const idx = queue.findIndex((entry) => entry.path === path && entry.status === "Active");
    if (idx >= 0) {
      markQueueStatus(idx, status);
    }
  }

  async function startNextPending(): Promise<void> {
    if (isRunning || queueDispatchLock) {
      return;
    }

    const nextIndex = queue.findIndex((entry) => entry.status === "Pending");
    if (nextIndex < 0) {
      activeQueueIndex = null;
      return;
    }

    const nextTarget = queue[nextIndex];
    activeQueueIndex = nextIndex;
    markQueueStatus(nextIndex, "Active");
    circuits = {};
    speedMbps = 0;
    isRunning = true;
    queueDispatchLock = true;
    addLog(`[+] Dispatching: ${nextTarget.url}`);

    try {
      await invoke("initiate_download", {
        args: {
          url: nextTarget.url,
          path: nextTarget.path,
          connections: Number(connections),
          force_tor: forceTor,
        },
      });
      addLog(`[*] Worker accepted target: ${nextTarget.path}`);
    } catch (error) {
      addLog(`[ERROR] Dispatcher failure: ${String(error)}`);
      markQueueStatus(nextIndex, "Failed");
      activeQueueIndex = null;
      isRunning = false;
    } finally {
      queueDispatchLock = false;
      if (!isRunning) {
        void startNextPending();
      }
    }
  }

  async function refreshArtifacts(): Promise<void> {
    const normalized = normalizeOutputDirectory(outputDir).replace(/[\\/]$/, "");
    treeLoading = true;
    treeError = "";

    try {
      const entries = await invoke<FileTreeEntry[]>("list_output_tree", {
        root: normalized,
        maxEntries: 1800,
      });
      outputEntries = entries;
      if (selectedEntry) {
        selectedEntry = outputEntries.find((entry) => entry.path === selectedEntry?.path) ?? null;
      }
    } catch (error) {
      outputEntries = [];
      treeError = String(error);
    } finally {
      treeLoading = false;
    }
  }

  async function selectEntry(entry: FileTreeEntry): Promise<void> {
    selectedEntry = entry;
    preview = null;
    previewError = "";
    previewLoading = false;

    if (entry.is_dir || isImageFile(entry)) {
      return;
    }

    previewLoading = true;
    try {
      preview = await invoke<FilePreview>("read_file_preview", {
        path: entry.path,
        maxBytes: 8192,
      });
    } catch (error) {
      previewError = String(error);
    } finally {
      previewLoading = false;
    }
  }

  onMount(() => {
    const unlisteners: UnlistenFn[] = [];
    let disposed = false;

    const register = async () => {
      const progressUnlisten = await listen<ProgressEvent>("progress", (event) => {
        circuits = { ...circuits, [event.payload.id]: event.payload };
      });
      if (disposed) {
        progressUnlisten();
        return;
      }
      unlisteners.push(progressUnlisten);

      const speedUnlisten = await listen<number>("speed", (event) => {
        speedMbps = event.payload;
      });
      if (disposed) {
        speedUnlisten();
        return;
      }
      unlisteners.push(speedUnlisten);

      const logUnlisten = await listen<string>("log", (event) => {
        addLog(event.payload);
      });
      if (disposed) {
        logUnlisten();
        return;
      }
      unlisteners.push(logUnlisten);

      const torUnlisten = await listen<TorStatusEvent>("tor_status", (event) => {
        torStatus = event.payload;
      });
      if (disposed) {
        torUnlisten();
        return;
      }
      unlisteners.push(torUnlisten);

      const completeUnlisten = await listen<CompleteEvent>("complete", (event) => {
        addLog(`[+] Complete: ${event.payload.path}`);
        addLog(`[+] SHA256: ${event.payload.hash}`);

        if (activeQueueIndex !== null) {
          markQueueStatus(activeQueueIndex, "Complete");
        } else {
          markByPath(event.payload.path, "Complete");
        }

        activeQueueIndex = null;
        isRunning = false;
        void refreshArtifacts();
        void startNextPending();
      });
      if (disposed) {
        completeUnlisten();
        return;
      }
      unlisteners.push(completeUnlisten);

      const failureUnlisten = await listen<DownloadFailedEvent>("download_failed", (event) => {
        addLog(`[ERROR] ${event.payload.error}`);
        if (activeQueueIndex !== null) {
          markQueueStatus(activeQueueIndex, "Failed");
        } else {
          markByPath(event.payload.path, "Failed");
        }
        activeQueueIndex = null;
        isRunning = false;
        void startNextPending();
      });
      if (disposed) {
        failureUnlisten();
        return;
      }
      unlisteners.push(failureUnlisten);

      await refreshArtifacts();
    };

    void register();

    return () => {
      disposed = true;
      for (const unlisten of unlisteners) {
        unlisten();
      }
    };
  });
</script>

<div class="relative min-h-screen overflow-hidden bg-slate-950 text-slate-100">
  <div class="pointer-events-none absolute inset-0">
    <div class="absolute -left-28 -top-28 h-72 w-72 rounded-full bg-cyan-400/20 blur-3xl"></div>
    <div class="absolute -right-24 top-32 h-72 w-72 rounded-full bg-violet-500/20 blur-3xl"></div>
    <div class="absolute bottom-0 left-1/3 h-80 w-80 rounded-full bg-sky-500/10 blur-3xl"></div>
    <div class="radar-grid absolute inset-0 opacity-35"></div>
  </div>

  <main class="relative mx-auto max-w-[1800px] space-y-6 p-4 md:p-6 lg:p-8">
    <header class="glass-card rounded-3xl px-6 py-5">
      <div class="flex flex-col gap-5 lg:flex-row lg:items-center lg:justify-between">
        <div class="space-y-2">
          <div class="font-display text-xs uppercase tracking-[0.32em] text-cyan-300/80">Tauri v2 Native Rust Engine</div>
          <h1 class="font-display text-3xl font-bold tracking-[0.12em] text-slate-50 md:text-4xl">
            LOKI ARIAFORGE COMMAND CONSOLE
          </h1>
          <p class="max-w-3xl text-sm text-slate-300/90">
            Multi-connection acquisition cockpit with dark-web routing controls, live circuit telemetry, and visual artifact explorer.
          </p>
        </div>
        <div class="flex flex-wrap items-center gap-3">
          <button
            class="rounded-xl bg-cyan-400/20 px-4 py-2 font-display text-xs uppercase tracking-[0.15em] text-cyan-200 ring-1 ring-cyan-300/40 transition hover:bg-cyan-300/30"
            on:click={refreshArtifacts}
            type="button"
          >
            Sync Artifacts
          </button>
          <div class={`rounded-xl px-4 py-2 font-display text-xs uppercase tracking-[0.12em] ring-1 ${torBadge(torStatus.state)}`}>
            TOR: {torStatus.state}
          </div>
        </div>
      </div>
    </header>

    <section class="grid gap-6 xl:grid-cols-[380px_minmax(0,1fr)_430px]">
      <div class="space-y-6">
        <article class="glass-card rounded-3xl p-5">
          <div class="mb-5 flex items-center justify-between">
            <h2 class="font-display text-sm uppercase tracking-[0.16em] text-slate-100">Mission Controls</h2>
            <div class="font-mono-ui text-xs text-cyan-300">SESSION // 0x7F41B</div>
          </div>

          <div class="space-y-4">
            <label class="block space-y-2 text-xs uppercase tracking-[0.12em] text-slate-300">
              Targets (one URL per line)
              <textarea
                bind:value={targetUrls}
                rows="5"
                class="font-mono-ui w-full rounded-2xl border border-slate-500/40 bg-slate-900/70 px-3 py-2 text-sm text-slate-100 outline-none ring-0 transition focus:border-cyan-400/60 focus:shadow-[0_0_0_1px_rgba(34,211,238,0.45)]"
                placeholder="https://example.com/archive.tar.gz&#10;http://target.onion/pack.7z"
                disabled={isRunning}
              ></textarea>
            </label>

            <label class="block space-y-2 text-xs uppercase tracking-[0.12em] text-slate-300">
              Output Directory
              <div class="flex gap-2">
                <input
                  bind:value={outputDir}
                  class="font-mono-ui min-w-0 flex-1 rounded-2xl border border-slate-500/40 bg-slate-900/70 px-3 py-2 text-sm text-slate-100 outline-none transition focus:border-cyan-400/60 focus:shadow-[0_0_0_1px_rgba(34,211,238,0.45)]"
                  disabled={isRunning}
                />
                <button
                  class="rounded-2xl bg-slate-800 px-3 text-xs font-semibold text-cyan-200 ring-1 ring-cyan-300/40 transition hover:bg-slate-700"
                  on:click={browseDirectory}
                  disabled={isRunning}
                  type="button"
                >
                  Browse
                </button>
              </div>
            </label>

            <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <label class="space-y-2 text-xs uppercase tracking-[0.12em] text-slate-300">
                Connections
                <input
                  type="number"
                  bind:value={connections}
                  min="1"
                  max="500"
                  class="font-mono-ui w-full rounded-2xl border border-slate-500/40 bg-slate-900/70 px-3 py-2 text-sm text-cyan-200 outline-none transition focus:border-cyan-400/60"
                  disabled={isRunning}
                />
              </label>
              <label class="space-y-2 text-xs uppercase tracking-[0.12em] text-slate-300">
                Tor Route
                <button
                  class={`w-full rounded-2xl px-3 py-2 font-display text-xs uppercase tracking-[0.1em] ring-1 transition ${
                    forceTor
                      ? "bg-cyan-400/20 text-cyan-200 ring-cyan-300/60"
                      : "bg-slate-900/70 text-slate-300 ring-slate-500/40 hover:ring-cyan-300/40"
                  }`}
                  on:click={() => (forceTor = !forceTor)}
                  disabled={isRunning}
                  type="button"
                >
                  {forceTor ? "Forced Tor" : "Auto Detect"}
                </button>
              </label>
            </div>

            <button
              class={`w-full rounded-2xl px-4 py-3 font-display text-sm uppercase tracking-[0.14em] ring-1 transition ${
                isRunning
                  ? "cursor-not-allowed bg-slate-700/60 text-slate-400 ring-slate-500/40"
                  : "bg-gradient-to-r from-cyan-500/30 to-violet-500/30 text-cyan-100 ring-cyan-300/60 hover:from-cyan-400/40 hover:to-violet-400/40"
              }`}
              on:click={engageQueue}
              type="button"
              disabled={isRunning}
            >
              {isRunning ? "Queue Running..." : "Launch Queue"}
            </button>
          </div>
        </article>

        <article class="glass-card rounded-3xl p-5">
          <h3 class="font-display text-sm uppercase tracking-[0.16em] text-slate-100">Queue Timeline</h3>
          <div class="mt-3 h-2 overflow-hidden rounded-full bg-slate-700/60">
            <div
              class="h-full rounded-full bg-gradient-to-r from-cyan-400 via-sky-400 to-violet-400 transition-all duration-500"
              style={`width: ${queueProgress}%`}
            ></div>
          </div>
          <div class="mt-2 flex items-center justify-between text-xs text-slate-300">
            <span>{queueProgress}% complete</span>
            <span>{completedQueue} done / {failedQueue} failed / {queue.length} total</span>
          </div>

          <div class="scroll-clean mt-4 max-h-[360px] space-y-2 overflow-auto pr-1">
            {#if queue.length === 0}
              <div class="rounded-2xl border border-dashed border-slate-500/50 bg-slate-900/50 p-4 text-sm text-slate-400">
                Queue is empty. Add targets and launch.
              </div>
            {:else}
              {#each queue as item, index (item.path)}
                <div class="rounded-2xl border border-slate-600/50 bg-slate-900/55 p-3">
                  <div class="flex items-center justify-between gap-3">
                    <div class="font-mono-ui text-xs text-slate-300">#{index + 1}</div>
                    <div class={`rounded-full px-2.5 py-1 text-[10px] uppercase tracking-[0.12em] ring-1 ${queueBadge(item.status)}`}>
                      {item.status}
                    </div>
                  </div>
                  <div class="mt-1 break-words font-mono-ui text-[11px] text-cyan-100">{item.url}</div>
                  <div class="mt-1 break-words font-mono-ui text-[10px] text-slate-400">{item.path}</div>
                </div>
              {/each}
            {/if}
          </div>
        </article>
      </div>

      <div class="space-y-6">
        <article class="glass-card rounded-3xl p-5">
          <h2 class="font-display text-sm uppercase tracking-[0.16em] text-slate-100">Live Telemetry</h2>
          <div class="mt-4 grid gap-3 sm:grid-cols-3">
            <div class="rounded-2xl border border-cyan-400/20 bg-slate-900/65 p-3">
              <div class="text-xs uppercase tracking-[0.12em] text-slate-400">Throughput</div>
              <div class="mt-1 font-display text-2xl text-cyan-300">{speedMbps.toFixed(2)} MB/s</div>
            </div>
            <div class="rounded-2xl border border-violet-400/20 bg-slate-900/65 p-3">
              <div class="text-xs uppercase tracking-[0.12em] text-slate-400">Transferred</div>
              <div class="mt-1 font-display text-2xl text-violet-300">{totalMb} MB</div>
            </div>
            <div class="rounded-2xl border border-emerald-400/20 bg-slate-900/65 p-3">
              <div class="text-xs uppercase tracking-[0.12em] text-slate-400">Active Circuits</div>
              <div class="mt-1 font-display text-2xl text-emerald-300">{activeCircuits}</div>
            </div>
          </div>

          <div class="mt-4 rounded-2xl border border-slate-600/50 bg-slate-900/50 p-3">
            <div class="flex items-center justify-between gap-3">
              <div>
                <div class="font-display text-xs uppercase tracking-[0.12em] text-slate-300">Tor Node State</div>
                <div class="mt-1 text-sm text-slate-200">{torStatus.message}</div>
              </div>
              <div class={`rounded-xl px-3 py-1.5 text-xs uppercase tracking-[0.12em] ring-1 ${torBadge(torStatus.state)}`}>
                {torStatus.state}
              </div>
            </div>
            <div class="mt-2 font-mono-ui text-xs text-slate-400">daemons: {torStatus.daemon_count}</div>
          </div>
        </article>

        <article class="glass-card rounded-3xl p-5">
          <div class="mb-4 flex items-center justify-between">
            <h2 class="font-display text-sm uppercase tracking-[0.16em] text-slate-100">Circuit Matrix</h2>
            <div class="font-mono-ui text-xs text-slate-400">slots: {connections}</div>
          </div>

          <div class="scroll-clean grid max-h-[430px] gap-3 overflow-auto pr-1 sm:grid-cols-2">
            {#if circuitList.length === 0}
              <div class="col-span-full rounded-2xl border border-dashed border-slate-500/50 bg-slate-900/50 p-6 text-center text-sm text-slate-400">
                Awaiting active circuits...
              </div>
            {:else}
              {#each circuitList as circuit (circuit.id)}
                <div class="rounded-2xl border border-slate-600/60 bg-slate-900/60 p-3">
                  <div class="flex items-center justify-between">
                    <div class="font-display text-xs uppercase tracking-[0.12em] text-cyan-300">Circuit {circuit.id + 1}</div>
                    <div class={`rounded-full px-2 py-0.5 text-[10px] uppercase tracking-[0.12em] ${
                      circuit.status === "Done" ? "bg-emerald-500/20 text-emerald-300" : "bg-cyan-500/20 text-cyan-300"
                    }`}>
                      {circuit.status}
                    </div>
                  </div>

                  <div class="mt-2 text-xs text-slate-300">
                    {formatBytes(circuit.downloaded)} / {formatBytes(circuit.total)}
                  </div>

                  <div class="mt-2 h-2 overflow-hidden rounded-full bg-slate-700/70">
                    <div
                      class="relative h-full overflow-hidden rounded-full bg-gradient-to-r from-cyan-400 to-sky-300 transition-all duration-300"
                      style={`width: ${Math.min(100, circuit.total > 0 ? (circuit.downloaded / circuit.total) * 100 : 0)}%`}
                    >
                      {#if circuit.status !== "Done"}
                        <div class="sweep absolute inset-y-0 w-10 bg-gradient-to-r from-transparent via-white/65 to-transparent"></div>
                      {/if}
                    </div>
                  </div>

                  <div class="mt-2 flex items-center justify-between text-xs text-slate-400">
                    <span>rate: {circuit.main_speed_mbps.toFixed(2)} MB/s</span>
                    <span>
                      {circuit.total > 0 ? ((circuit.downloaded / circuit.total) * 100).toFixed(1) : "0.0"}%
                    </span>
                  </div>
                </div>
              {/each}
            {/if}
          </div>
        </article>

        <article class="glass-card rounded-3xl p-5">
          <h2 class="font-display text-sm uppercase tracking-[0.16em] text-slate-100">Operational Log</h2>
          <div bind:this={logsContainer} class="scroll-clean mt-4 h-52 overflow-auto rounded-2xl border border-slate-600/60 bg-slate-950/70 p-3 font-mono-ui text-[11px] leading-relaxed text-slate-300">
            {#if logs.length === 0}
              <div class="text-slate-500">[SYSTEM] Waiting for directives...</div>
            {:else}
              {#each logs as line}
                <div class="mb-1 break-words">{line}</div>
              {/each}
            {/if}
          </div>
        </article>
      </div>

      <div class="space-y-6">
        <article class="glass-card rounded-3xl p-5">
          <div class="mb-4 flex items-center justify-between">
            <h2 class="font-display text-sm uppercase tracking-[0.16em] text-slate-100">Artifact Tree</h2>
            <button
              class="rounded-xl bg-slate-800 px-3 py-1.5 text-xs text-cyan-200 ring-1 ring-cyan-300/40 transition hover:bg-slate-700"
              on:click={refreshArtifacts}
              type="button"
            >
              Refresh
            </button>
          </div>

          <div class="mb-3 font-mono-ui text-[11px] text-slate-400">root: {outputDir}</div>
          <div class="scroll-clean max-h-[370px] space-y-1 overflow-auto rounded-2xl border border-slate-600/60 bg-slate-950/60 p-2">
            {#if treeLoading}
              <div class="p-3 text-sm text-slate-400">Scanning output directory...</div>
            {:else if treeError}
              <div class="p-3 text-sm text-rose-300">{treeError}</div>
            {:else if outputEntries.length === 0}
              <div class="p-3 text-sm text-slate-500">No artifacts yet.</div>
            {:else}
              {#each outputEntries as entry (entry.path)}
                <button
                  class={`flex w-full items-center justify-between gap-3 rounded-xl px-2 py-1.5 text-left transition ${
                    selectedEntry?.path === entry.path ? "bg-cyan-500/20 ring-1 ring-cyan-300/40" : "hover:bg-slate-800/80"
                  }`}
                  style={`padding-left: ${10 + entry.depth * 14}px`}
                  on:click={() => selectEntry(entry)}
                  type="button"
                >
                  <div class="min-w-0 flex-1">
                    <div class="truncate font-mono-ui text-[11px] text-slate-200">
                      <span class="mr-1 text-cyan-300">{classifyEntry(entry)}</span>
                      {entry.relative}
                    </div>
                    <div class="mt-0.5 text-[10px] text-slate-500">{entry.is_dir ? "directory" : formatBytes(entry.size)}</div>
                  </div>
                </button>
              {/each}
            {/if}
          </div>
        </article>

        <article class="glass-card rounded-3xl p-5">
          <h2 class="font-display text-sm uppercase tracking-[0.16em] text-slate-100">Preview</h2>

          {#if !selectedEntry}
            <div class="mt-4 rounded-2xl border border-dashed border-slate-600/60 bg-slate-950/50 p-5 text-sm text-slate-500">
              Select an artifact from the tree to inspect metadata or preview content.
            </div>
          {:else}
            <div class="mt-4 space-y-3">
              <div class="rounded-2xl border border-slate-600/60 bg-slate-950/60 p-3">
                <div class="font-mono-ui text-xs text-cyan-200">{selectedEntry.relative}</div>
                <div class="mt-2 grid grid-cols-1 gap-2 text-[11px] text-slate-400 sm:grid-cols-2">
                  <div>Type: {selectedEntry.is_dir ? "Directory" : selectedEntry.extension ?? "file"}</div>
                  <div>Size: {selectedEntry.is_dir ? "-" : formatBytes(selectedEntry.size)}</div>
                  <div class="sm:col-span-2">Modified: {formatTimestamp(selectedEntry.modified)}</div>
                </div>
              </div>

              {#if selectedEntry.is_dir}
                <div class="rounded-2xl border border-slate-600/60 bg-slate-950/60 p-4 text-sm text-slate-400">
                  Directory node selected. Choose a file for content preview.
                </div>
              {:else if selectedImageSrc}
                <div class="overflow-hidden rounded-2xl border border-slate-600/60 bg-slate-950/60 p-2">
                  <img src={selectedImageSrc} alt={selectedEntry.name} class="max-h-[320px] w-full rounded-xl object-contain" />
                </div>
              {:else if previewLoading}
                <div class="rounded-2xl border border-slate-600/60 bg-slate-950/60 p-4 text-sm text-slate-400">Loading preview...</div>
              {:else if previewError}
                <div class="rounded-2xl border border-rose-500/40 bg-rose-500/10 p-4 text-sm text-rose-200">{previewError}</div>
              {:else if preview}
                <div class="rounded-2xl border border-slate-600/60 bg-slate-950/75 p-3">
                  <div class="mb-2 text-[11px] text-slate-400">
                    {preview.kind} preview ({preview.bytes_read} bytes{preview.truncated ? ", truncated" : ""})
                  </div>
                  <pre class="scroll-clean max-h-[260px] overflow-auto whitespace-pre-wrap break-words font-mono-ui text-[11px] text-slate-200">{preview.content}</pre>
                </div>
              {:else}
                <div class="rounded-2xl border border-slate-600/60 bg-slate-950/60 p-4 text-sm text-slate-400">
                  No preview available for this file.
                </div>
              {/if}
            </div>
          {/if}
        </article>
      </div>
    </section>
  </main>
</div>
