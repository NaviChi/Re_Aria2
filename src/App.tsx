import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import "./App.css";

interface ProgressEvent {
  id: number;
  downloaded: number;
  total: number;
  main_speed_mbps: number;
  status: string;
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

interface QueuedItem {
  url: string;
  path: string;
  status: "Pending" | "Active" | "Complete" | "Failed";
}

function App() {
  const [targetUrls, setTargetUrls] = useState("");
  const [outputDir, setOutputDir] = useState("/tmp/loki_out/");
  const [connections, setConnections] = useState(150);
  const [forceTor, setForceTor] = useState(false);

  const [isRunning, setIsRunning] = useState(false);
  const [speed, setSpeed] = useState("0.00");
  const [logs, setLogs] = useState<string[]>([]);
  const [circuits, setCircuits] = useState<Record<number, ProgressEvent>>({});
  const [queue, setQueue] = useState<QueuedItem[]>([]);
  const [activeQueueIndex, setActiveQueueIndex] = useState<number | null>(null);
  const [torStatus, setTorStatus] = useState<TorStatusEvent>({
    state: "idle",
    message: "Tor inactive until onion links or force mode.",
    daemon_count: 0,
  });

  const logsEndRef = useRef<HTMLDivElement>(null);
  const activeQueueIndexRef = useRef<number | null>(null);

  const updateActiveQueueIndex = (index: number | null) => {
    activeQueueIndexRef.current = index;
    setActiveQueueIndex(index);
  };

  useEffect(() => {
    logsEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [logs]);

  useEffect(() => {
    let destroyed = false;
    const unlisteners: Array<() => void> = [];

    const setupListeners = async () => {
      const unlistenProgress = await listen<ProgressEvent>("progress", (e) => {
        setCircuits((prev) => ({
          ...prev,
          [e.payload.id]: e.payload,
        }));
      });
      if (destroyed) {
        unlistenProgress();
        return;
      }
      unlisteners.push(unlistenProgress);

      const unlistenLog = await listen<string>("log", (e) => {
        setLogs((prev) => [...prev, e.payload]);
      });
      if (destroyed) {
        unlistenLog();
        return;
      }
      unlisteners.push(unlistenLog);

      const unlistenSpeed = await listen<number>("speed", (e) => {
        setSpeed(e.payload.toFixed(2));
      });
      if (destroyed) {
        unlistenSpeed();
        return;
      }
      unlisteners.push(unlistenSpeed);

      const unlistenTorStatus = await listen<TorStatusEvent>("tor_status", (e) => {
        setTorStatus(e.payload);
      });
      if (destroyed) {
        unlistenTorStatus();
        return;
      }
      unlisteners.push(unlistenTorStatus);

      const unlistenComplete = await listen<CompleteEvent>("complete", (e) => {
        setLogs((prev) => [
          ...prev,
          `[!] Integrity Verified: ${e.payload.hash}`,
          `[+] Saved to: ${e.payload.path}`,
        ]);

        const activeIndex = activeQueueIndexRef.current;
        if (activeIndex !== null) {
          setQueue((prev) =>
            prev.map((q, i) => (i === activeIndex ? { ...q, status: "Complete" } : q)),
          );
        } else {
          setQueue((prev) => {
            const idx = prev.findIndex((q) => q.path === e.payload.path && q.status === "Active");
            if (idx < 0) {
              return prev;
            }
            return prev.map((q, i) => (i === idx ? { ...q, status: "Complete" } : q));
          });
        }

        updateActiveQueueIndex(null);
        setIsRunning(false);
      });
      if (destroyed) {
        unlistenComplete();
        return;
      }
      unlisteners.push(unlistenComplete);

      const unlistenFailed = await listen<DownloadFailedEvent>("download_failed", (e) => {
        setLogs((prev) => [...prev, `[ERROR] ${e.payload.error}`]);

        const activeIndex = activeQueueIndexRef.current;
        if (activeIndex !== null) {
          setQueue((prev) =>
            prev.map((q, i) => (i === activeIndex ? { ...q, status: "Failed" } : q)),
          );
        } else {
          setQueue((prev) => {
            const idx = prev.findIndex((q) => q.path === e.payload.path && q.status === "Active");
            if (idx < 0) {
              return prev;
            }
            return prev.map((q, i) => (i === idx ? { ...q, status: "Failed" } : q));
          });
        }

        updateActiveQueueIndex(null);
        setIsRunning(false);
      });
      if (destroyed) {
        unlistenFailed();
        return;
      }
      unlisteners.push(unlistenFailed);
    };

    void setupListeners();

    return () => {
      destroyed = true;
      for (const unlisten of unlisteners) {
        unlisten();
      }
    };
  }, []);

  useEffect(() => {
    if (isRunning || activeQueueIndex !== null || queue.length === 0) {
      return;
    }

    const pendingIndex = queue.findIndex((q) => q.status === "Pending");
    if (pendingIndex > -1) {
      void processNextInQueue(pendingIndex);
    }
  }, [isRunning, queue, activeQueueIndex]);

  const processNextInQueue = async (index: number) => {
    const item = queue[index];
    if (!item) {
      return;
    }

    updateActiveQueueIndex(index);
    setQueue((prev) => prev.map((q, i) => (i === index ? { ...q, status: "Active" } : q)));
    setIsRunning(true);
    setCircuits({});
    setSpeed("0.00");
    setLogs((prev) => [...prev, `[+] Engaging Automatic Extraction for: ${item.url}`]);

    try {
      await invoke("initiate_download", {
        args: {
          url: item.url,
          path: item.path,
          connections: Number(connections),
          force_tor: forceTor,
        },
      });
    } catch (err) {
      setLogs((prev) => [...prev, `[ERROR] ${String(err)}`]);
      setQueue((prev) => prev.map((q, i) => (i === index ? { ...q, status: "Failed" } : q)));
      updateActiveQueueIndex(null);
      setIsRunning(false);
    }
  };

  const handleBrowseDir = async () => {
    const selected = await open({
      directory: true,
      multiple: false,
      title: "Select Output Directory",
    });
    if (selected && typeof selected === "string") {
      setOutputDir(selected + (selected.endsWith("/") || selected.endsWith("\\") ? "" : "/"));
    }
  };

  const handleStart = () => {
    if (!targetUrls || !outputDir) {
      return;
    }

    const urls = targetUrls
      .split("\n")
      .map((line) => line.trim())
      .filter((line) => line.length > 0);

    const newQueue: QueuedItem[] = urls.map((url, i) => {
      let filename = url.split("/").pop();
      if (!filename) {
        filename = `file_${i}.bin`;
      }
      const fullPath = outputDir.endsWith("/") ? `${outputDir}${filename}` : `${outputDir}/${filename}`;
      return { url, path: fullPath, status: "Pending" };
    });

    const onionRequested = forceTor || urls.some((url) => url.includes(".onion"));
    setTorStatus(
      onionRequested
        ? {
            state: "starting",
            message: "Queued Tor-enabled transfer. Waiting for engine kickoff...",
            daemon_count: 0,
          }
        : {
            state: "clearnet",
            message: "Queued clearnet transfer. Tor will remain disabled.",
            daemon_count: 0,
          },
    );

    updateActiveQueueIndex(null);
    setQueue(newQueue);
  };

  const totalBytes = Object.values(circuits).reduce((acc, curr) => acc + curr.downloaded, 0);
  const totalMB = (totalBytes / 1048576).toFixed(2);

  return (
    <div className="container">
      <div className="cyber-grid"></div>

      <header className="header">
        <div className="title">
          <svg
            width="40"
            height="40"
            viewBox="0 0 24 24"
            fill="none"
            stroke="url(#cyan-green-grad)"
            strokeWidth="2"
            strokeLinecap="square"
            style={{ filter: "drop-shadow(0 0 10px rgba(184, 41, 255, 0.5))" }}
          >
            <defs>
              <linearGradient id="cyan-green-grad" x1="0" y1="0" x2="1" y2="1">
                <stop offset="0%" stopColor="#00e5ff" />
                <stop offset="100%" stopColor="#b829ff" />
              </linearGradient>
            </defs>
            <path d="M13 2L3 14h9l-1 8 10-12h-9l1-8z"></path>
          </svg>
          <div className="title-text">
            <h1 className="cyber-glitch">LOKI ARIAFORGE</h1>
            <div className="subtitle">
              ((p)) ONLINE - SECURE <span className="pips">▮▮▮▮</span>
            </div>
          </div>
        </div>
        <div className="header-controls">
          <button className="tab-btn active">AUTO SWARM TARGETER</button>
        </div>
      </header>

      <main className="dashboard">
        <div className="hud-layout">
          <section className="left-panel">
            <div className="cyber-panel">
              <div className="panel-header">
                <h2>CONFIGURATION & GOVERNOR</h2>
                <span className="close-btn">X</span>
              </div>
              <div className="panel-content">
                <div className="input-row">
                  <label>SESSION ID</label>
                  <input type="text" value="0x7F41B" disabled />
                </div>
                <div className="input-row" style={{ flexDirection: "column", alignItems: "flex-start" }}>
                  <label style={{ marginBottom: "0.5rem" }}>TARGET URL(S) - AUTO DETECT</label>
                  <textarea
                    value={targetUrls}
                    onChange={(e) => setTargetUrls(e.target.value)}
                    placeholder="Provide 1 or 1,000 URLs here..."
                    disabled={isRunning}
                    rows={3}
                    style={{ width: "100%", resize: "vertical" }}
                  />
                </div>
                <div className="input-row">
                  <label>OUTPUT DIR</label>
                  <div className="input-with-btn">
                    <input type="text" value={outputDir} onChange={(e) => setOutputDir(e.target.value)} disabled={isRunning} />
                    <button className="btn-browse" onClick={handleBrowseDir} disabled={isRunning}>
                      ...
                    </button>
                  </div>
                </div>
                <div className="input-row">
                  <label>MULTIPLEX LIMIT</label>
                  <input
                    type="number"
                    value={connections}
                    onChange={(e) => setConnections(Number(e.target.value))}
                    min="1"
                    max="500"
                    disabled={isRunning}
                    style={{ color: "var(--success)" }}
                  />
                </div>
                <div className="input-row checkbox-row">
                  <label>FORCE TOR</label>
                  <input
                    type="checkbox"
                    checked={forceTor}
                    onChange={(e) => setForceTor(e.target.checked)}
                    disabled={isRunning}
                  />
                </div>
                <div style={{ flex: 1 }}></div>
                <button className={`btn-engage ${isRunning ? "active" : ""}`} onClick={handleStart} disabled={isRunning || !targetUrls}>
                  <span className="btn-text">ENGAGE GOVERNOR</span>
                  <span className="btn-status">{isRunning ? "ACTIVE" : "STANDBY"}</span>
                </button>
              </div>
            </div>

            <div className="cyber-panel tor-panel">
              <div className="panel-header">
                <h2>TOR BOOTSTRAP STATUS</h2>
                <span className={`tor-pill ${torStatus.state}`}>{torStatus.state.toUpperCase()}</span>
              </div>
              <div className="panel-content tor-content">
                <div className="tor-row">
                  <span>MODE</span>
                  <strong>{forceTor ? "FORCED" : "AUTO"}</strong>
                </div>
                <div className="tor-row">
                  <span>DAEMONS</span>
                  <strong>{torStatus.daemon_count}</strong>
                </div>
                <div className="tor-msg">{torStatus.message}</div>
              </div>
            </div>

            <div className="cyber-panel terminal-wrapper">
              <div className="panel-header">
                <h2>DIAGNOSTIC HACKING TERMINAL</h2>
                <span className="close-btn">_ [] X</span>
              </div>
              <div className="panel-content terminal">
                {logs.length === 0 && <span style={{ color: "var(--text-muted)" }}>[SYSTEM] AWAITING COMMAND...</span>}
                {logs.map((log, i) => {
                  const timestamp = new Date().toISOString().substring(11, 19);
                  return (
                    <span key={i} className={log.includes("[+]") || log.includes("Verified") ? "success" : log.includes("[ERROR]") ? "error" : "highlight"}>
                      <span style={{ color: "var(--text-muted)", marginRight: "8px" }}>[{timestamp}]</span>
                      {log}
                    </span>
                  );
                })}
                <div ref={logsEndRef} />
                <span className="prompt">
                  root@ariaforge:~# <span className="cursor">█</span>
                </span>
              </div>
            </div>
          </section>

          <section className="right-panel">
            <div className="cyber-panel grid-panel">
              <div className="panel-header">
                <h2>GEOGRAPHIC CIRCUIT NODES</h2>
                <div className="header-stats">
                  <span>
                    THROUGHPUT: <strong style={{ color: "var(--success)" }}>{speed} MB/s</strong>
                  </span>
                  <span>
                    TOTAL: <strong>{totalMB} MB</strong>
                  </span>
                </div>
              </div>
              <div className="panel-content nodes-grid">
                {Object.keys(circuits).length > 0 ? (
                  Array.from({ length: connections }).map((_, i) => {
                    const circuit = circuits[i];
                    if (!circuit) {
                      return null;
                    }

                    const progress = circuit.total > 0 ? (circuit.downloaded / circuit.total) * 100 : 0;
                    const isDone = circuit.status === "Done";
                    const locations = ["UK-NORD", "DE-BER", "JP-TKY", "US-NY", "RU-MSK", "NL-AMS"];
                    const mockLoc = `${locations[i % locations.length]}-${(i + 1).toString().padStart(2, "0")}`;

                    return (
                      <div key={i} className="node-card">
                        <div className="node-top">
                          <span className="n-id">{mockLoc}</span>
                          <span className={`n-state ${isDone ? "done" : "active"}`}>{isDone ? "VERIFIED" : "ACTIVE"}</span>
                        </div>

                        <div className="node-map-placeholder">
                          <div className="map-dots">...:::...::....</div>
                        </div>

                        <div className="node-stats-row">
                          <div className="stat-col">
                            <span className="s-label">PROGRESS</span>
                            <span className="s-val">{progress.toFixed(0)}%</span>
                          </div>
                          <div className="node-progress">
                            <div className={`bar ${isDone ? "done" : ""}`} style={{ width: `${Math.min(100, Math.max(0, progress))}%` }}></div>
                          </div>
                        </div>

                        <div className="node-stats-row">
                          <div className="stat-col">
                            <span className="s-label">SPEED</span>
                            <span className="s-val">{(circuit.main_speed_mbps || 0).toFixed(1)} MB/s</span>
                          </div>
                          <div className="stat-col right">
                            <span className="s-label">STATUS</span>
                            <span className={`s-val ${isDone ? "done" : "stable"}`}>{isDone ? "SECURE" : "STABLE"}</span>
                          </div>
                        </div>
                      </div>
                    );
                  })
                ) : (
                  <div
                    style={{
                      padding: "2rem",
                      color: "var(--text-muted)",
                      textAlign: "center",
                      gridColumn: "1 / -1",
                      fontFamily: "Orbitron",
                    }}
                  >
                    AWAITING CONNECTION TARGET...
                  </div>
                )}
              </div>
            </div>

            {queue.length > 0 && (
              <div className="cyber-panel file-queue">
                <div className="panel-header">
                  <h2>SWARM QUEUE STATUS</h2>
                </div>
                <div className="panel-content queue-list">
                  {queue.map((q, idx) => (
                    <div key={idx} className={`q-item ${q.status.toLowerCase()} ${idx === activeQueueIndex ? "focus" : ""}`}>
                      <span className="q-url">{q.url}</span>
                      <span className={`q-status ${q.status.toLowerCase()}`}>{q.status}</span>
                    </div>
                  ))}
                </div>
              </div>
            )}
          </section>
        </div>
      </main>
    </div>
  );
}

export default App;
