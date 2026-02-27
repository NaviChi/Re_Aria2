import { useState, useEffect, useRef } from "react";
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

interface QueuedItem {
  url: string;
  path: string;
  status: 'Pending' | 'Active' | 'Complete' | 'Failed';
}

function App() {
  // Targets
  const [targetUrls, setTargetUrls] = useState("");
  const [outputDir, setOutputDir] = useState("/tmp/loki_out/");

  // Shared config
  const [connections, setConnections] = useState(150);
  const [forceTor, setForceTor] = useState(false);

  // Engine State
  const [isRunning, setIsRunning] = useState(false);
  const [speed, setSpeed] = useState("0.00");
  const [logs, setLogs] = useState<string[]>([]);
  const [circuits, setCircuits] = useState<Record<number, ProgressEvent>>({});

  // Queue Manger
  const [queue, setQueue] = useState<QueuedItem[]>([]);

  const logsEndRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    logsEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [logs]);

  useEffect(() => {
    const setupListeners = async () => {
      const unlistenProgress = await listen<ProgressEvent>("progress", (e) => {
        setCircuits((prev) => ({
          ...prev,
          [e.payload.id]: e.payload,
        }));
      });

      const unlistenLog = await listen<string>("log", (e) => {
        setLogs((prev) => [...prev, e.payload]);
      });

      const unlistenSpeed = await listen<number>("speed", (e) => {
        setSpeed(e.payload.toFixed(2));
      });

      const unlistenComplete = await listen<string>("complete", (e) => {
        setLogs((prev) => [...prev, "[!] Integrity Verified: " + e.payload]);
        // The queue processor effect will pick up the next item when isRunning drops
        setIsRunning(false);
      });

      return () => {
        unlistenProgress();
        unlistenLog();
        unlistenSpeed();
        unlistenComplete();
      };
    };

    setupListeners();
  }, []);

  // Queue Processor
  useEffect(() => {
    if (!isRunning && queue.length > 0) {
      const pendingIndex = queue.findIndex(q => q.status === 'Pending');
      if (pendingIndex > -1) {
        processNextInQueue(pendingIndex);
      }
    }
  }, [isRunning, queue]);

  const processNextInQueue = async (index: number) => {
    const item = queue[index];

    // Update queue state
    setQueue(prev => prev.map((q, i) => i === index ? { ...q, status: 'Active' } : q));
    setIsRunning(true);
    setLogs([`[+] Engaging Automatic Extraction for: ${item.url}`]);
    setCircuits({});
    setSpeed("0.00");

    try {
      await invoke("initiate_download", {
        args: {
          url: item.url,
          path: item.path,
          connections: Number(connections),
          force_tor: forceTor,
        }
      });
      // The rust engine will emit "complete" which turns isRunning false and triggers the next item
      setQueue(prev => prev.map((q, i) => i === index ? { ...q, status: 'Complete' } : q));
    } catch (err) {
      setLogs((prev) => [...prev, `[ERROR] ${err}`]);
      setQueue(prev => prev.map((q, i) => i === index ? { ...q, status: 'Failed' } : q));
      setIsRunning(false); // trigger next
    }
  }

  const handleBrowseDir = async () => {
    const selected = await open({
      directory: true,
      multiple: false,
      title: "Select Output Directory",
    });
    if (selected && typeof selected === 'string') {
      setOutputDir(selected + (selected.endsWith('/') || selected.endsWith('\\') ? '' : '/'));
    }
  };

  const handleStart = () => {
    if (!targetUrls || !outputDir) return;
    const urls = targetUrls.split('\n').map(l => l.trim()).filter(l => l.length > 0);
    const newQueue: QueuedItem[] = urls.map((u, i) => {
      let filename = u.split('/').pop();
      if (!filename) filename = `file_${i}.bin`;

      const fullPath = outputDir.endsWith('/') ? `${outputDir}${filename}` : `${outputDir}/${filename}`;
      return { url: u, path: fullPath, status: 'Pending' };
    });
    setQueue(newQueue);
  };

  const totalBytes = Object.values(circuits).reduce((acc, curr) => acc + curr.downloaded, 0);
  const totalMB = (totalBytes / 1048576).toFixed(2);

  return (
    <div className="container">
      <div className="cyber-grid"></div>

      <header className="header">
        <div className="title">
          <svg width="40" height="40" viewBox="0 0 24 24" fill="none" stroke="url(#cyan-green-grad)" strokeWidth="2" strokeLinecap="square" style={{ filter: 'drop-shadow(0 0 10px rgba(184, 41, 255, 0.5))' }}>
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
            <div className="subtitle">((p)) ONLINE - SECURE <span className="pips">▮▮▮▮</span></div>
          </div>
        </div>
        <div className="header-controls">
          <button className="tab-btn active">AUTO SWARM TARGETER</button>
        </div>
      </header>

      <main className="dashboard">
        <div className="hud-layout">
          {/* Left HUD Column */}
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
                <div className="input-row" style={{ flexDirection: 'column', alignItems: 'flex-start' }}>
                  <label style={{ marginBottom: '0.5rem' }}>TARGET URL(S) - AUTO DETECT</label>
                  <textarea
                    value={targetUrls}
                    onChange={(e) => setTargetUrls(e.target.value)}
                    placeholder="Provide 1 or 1,000 URLs here..."
                    disabled={isRunning}
                    rows={3}
                    style={{ width: '100%', resize: 'vertical' }}
                  />
                </div>
                <div className="input-row">
                  <label>OUTPUT DIR</label>
                  <div className="input-with-btn">
                    <input type="text" value={outputDir} onChange={(e) => setOutputDir(e.target.value)} disabled={isRunning} />
                    <button className="btn-browse" onClick={handleBrowseDir} disabled={isRunning}>...</button>
                  </div>
                </div>

                <div className="input-row">
                  <label>MULTIPLEX LIMIT</label>
                  <input
                    type="number"
                    value={connections}
                    onChange={(e) => setConnections(Number(e.target.value))}
                    min="1" max="500"
                    disabled={isRunning}
                    style={{ color: 'var(--success)' }}
                  />
                </div>

                <div className="input-row checkbox-row">
                  <label>TOR DAEMONS</label>
                  <input
                    type="checkbox"
                    checked={forceTor}
                    onChange={(e) => setForceTor(e.target.checked)}
                    disabled={isRunning}
                  />
                </div>

                <div style={{ flex: 1 }}></div>

                <button
                  className={`btn-engage ${isRunning ? 'active' : ''}`}
                  onClick={handleStart}
                  disabled={isRunning || !targetUrls}
                >
                  <span className="btn-text">ENGAGE GOVERNOR</span>
                  <span className="btn-status">{isRunning ? 'ACTIVE' : 'STANDBY'}</span>
                </button>
              </div>
            </div>

            <div className="cyber-panel terminal-wrapper">
              <div className="panel-header">
                <h2>DIAGNOSTIC HACKING TERMINAL</h2>
                <span className="close-btn">_ [] X</span>
              </div>
              <div className="panel-content terminal">
                {logs.length === 0 && <span style={{ color: 'var(--text-muted)' }}>[SYSTEM] AWAITING COMMAND...</span>}
                {logs.map((log, i) => {
                  const timestamp = new Date().toISOString().substring(11, 19);
                  return (
                    <span key={i} className={log.includes('[+]') || log.includes('Verified') ? 'success' : log.includes('[ERROR]') ? 'error' : 'highlight'}>
                      <span style={{ color: 'var(--text-muted)', marginRight: '8px' }}>[{timestamp}]</span>
                      {log}
                    </span>
                  )
                })}
                <div ref={logsEndRef} />
                <span className="prompt">root@ariaforge:~# <span className="cursor">█</span></span>
              </div>
            </div>
          </section>

          {/* Right HUD Column */}
          <section className="right-panel">
            <div className="cyber-panel grid-panel">
              <div className="panel-header">
                <h2>GEOGRAPHIC CIRCUIT NODES</h2>
                <div className="header-stats">
                  <span>THROUGHPUT: <strong style={{ color: 'var(--success)' }}>{speed} MB/s</strong></span>
                  <span>TOTAL: <strong>{totalMB} MB</strong></span>
                </div>
              </div>
              <div className="panel-content nodes-grid">
                {Object.keys(circuits).length > 0 ? Array.from({ length: connections }).map((_, i) => {
                  const circuit = circuits[i];
                  if (!circuit) return null;

                  const progress = circuit.total > 0 ? (circuit.downloaded / circuit.total) * 100 : 0;
                  const isDone = circuit.status === "Done";

                  // Cyberpunk mock locations
                  const locations = ["UK-NORD", "DE-BER", "JP-TKY", "US-NY", "RU-MSK", "NL-AMS"];
                  const mockLoc = `${locations[i % locations.length]}-${(i + 1).toString().padStart(2, '0')}`;

                  return (
                    <div key={i} className="node-card">
                      <div className="node-top">
                        <span className="n-id">{mockLoc}</span>
                        <span className={`n-state ${isDone ? 'done' : 'active'}`}>{isDone ? 'VERIFIED' : 'ACTIVE'}</span>
                      </div>

                      <div className="node-map-placeholder">
                        {/* World map stylized placeholder */}
                        <div className="map-dots">...:::...::....</div>
                      </div>

                      <div className="node-stats-row">
                        <div className="stat-col">
                          <span className="s-label">PROGRESS</span>
                          <span className="s-val">{progress.toFixed(0)}%</span>
                        </div>
                        <div className="node-progress">
                          <div className={`bar ${isDone ? 'done' : ''}`} style={{ width: `${Math.min(100, Math.max(0, progress))}%` }}></div>
                        </div>
                      </div>

                      <div className="node-stats-row">
                        <div className="stat-col">
                          <span className="s-label">SPEED</span>
                          <span className="s-val">{(circuit.main_speed_mbps || 0).toFixed(1)} MB/s</span>
                        </div>
                        <div className="stat-col right">
                          <span className="s-label">STATUS</span>
                          <span className={`s-val ${isDone ? 'done' : 'stable'}`}>{isDone ? 'SECURE' : 'STABLE'}</span>
                        </div>
                      </div>
                    </div>
                  );
                }) : (
                  <div style={{ padding: '2rem', color: 'var(--text-muted)', textAlign: 'center', gridColumn: '1 / -1', fontFamily: 'Orbitron' }}>
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
                    <div key={idx} className={`q-item ${q.status.toLowerCase()}`}>
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
