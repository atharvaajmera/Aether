import React, { useState, useEffect, useRef } from "react";
import { File, RefreshCcw, Monitor, Wifi, Globe, CheckCircle2 } from "lucide-react";
import { open } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import { PlenumEventEnvelope, DiscoverRequest, DiscoverySummary, SendRequest, SendRemoteRequest, TransferSummary, TransferEvent, IceServer, TransferUiPhase } from "../types/rust";
import { addHistoryEntry } from "../services/history";
import { formatBytes, formatDuration, progressPercent } from "../utils/format";
import { isStaleSession, abandonSession } from "../utils/session";
import { createTransferMetrics, updateTransferMetrics, TransferMetricsState } from "../utils/transferMetrics";
import { lookupRoomWithGracePeriod, roomLookupMessage } from "../utils/relayUrl";
import { useSettings } from "../context/SettingsContext";
import { RELAY_SERVER_URL, DEFAULT_ICE_SERVERS } from "../config";

type LogEvent = { level: string; message: string };

const logToConsole = (log: LogEvent) => {
  if (log.level === "Error") {
    console.error(log.message);
  } else if (log.level === "Warn") {
    console.warn(log.message);
  } else {
    console.info(log.message);
  }
};

const STATE_LABELS: Record<string, string> = {
  Discovering: "Searching...",
  Listening: "Ready to receive files",
  Connecting: "Establishing secure connection…",
  SignalingConnected: "Establishing secure connection…",
  NegotiatingIce: "Establishing secure connection…",
  Connected: "Establishing secure connection…",
};

const friendlyState = (state: string): string => STATE_LABELS[state] ?? "Establishing secure connection…";

const SendPage: React.FC = () => {
  const { settings } = useSettings();
  const [mode, setMode] = useState<"local" | "internet">("local");
  const [phase, setPhase] = useState<TransferUiPhase>("idle");
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [peers, setPeers] = useState<DiscoverySummary[]>([]);
  const [isDiscovering, setIsDiscovering] = useState(false);
  const [transferStatus, setTransferStatus] = useState<string>("");
  const [progress, setProgress] = useState<{ transferred: number, total: number } | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const [pinInputPeer, setPinInputPeer] = useState<DiscoverySummary | null>(null);
  const [pinInput, setPinInput] = useState("");
  const [roomCodeInput, setRoomCodeInput] = useState("");
  const [isConnectingRemote, setIsConnectingRemote] = useState(false);
  // True for the whole duration of any send (local or remote)
  const [isTransferActive, setIsTransferActive] = useState(false);
  const [sendSuccess, setSendSuccess] = useState(false);
  const [speedText, setSpeedText] = useState<string | null>(null);
  const [etaText, setEtaText] = useState<string | null>(null);
  // Auto-reset timer so a late Completed can't race a fresh transfer's UI.
  const autoResetRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const metricsRef = useRef<TransferMetricsState | null>(null);
  const terminalEventRef = useRef(false);
  const activeSessionRef = useRef(0);
  const sessionFloorRef = useRef(0);
  // Discovery is a local-mode-only activity. The listener closes over the first
  // render, so it reads current mode through this ref instead of `mode`.
  const modeRef = useRef(mode);
  // Bumped whenever an in-flight discovery is superseded (new search) or
  // invalidated (mode switch / unmount) so its late completion can't flip state.
  const discoveryGenRef = useRef(0);

  const startDiscovery = async () => {
    // Only local mode discovers; ignore stray calls from other modes.
    if (modeRef.current !== "local") return;
    const gen = ++discoveryGenRef.current;
    setIsDiscovering(true);
    setPhase("discovering");
    setPeers([]);

    try {
      const req: DiscoverRequest = {
        timeout_secs: 10,
        permissions: { local_network: true, file_system_read: true, file_system_write: true, background_transfer: false }
      };
      await invoke<DiscoverySummary>("discover_peers_command", { request: req });
    } catch (err) {
      console.error("Discovery error:", err);
    } finally {
      // Skip if this run was superseded/invalidated (mode switch, new search).
      if (discoveryGenRef.current === gen) {
        setIsDiscovering(false);
        setPhase((prev) => (prev === "discovering" ? "idle" : prev));
      }
    }
  };

  // Keep modeRef current for the long-lived event listener. Declared before the
  // discovery effect so modeRef is updated before that effect reads it.
  useEffect(() => { modeRef.current = mode; }, [mode]);

  // Discovery is scoped to local mode: start it on entering local, invalidate it
  // on leaving (or unmount) so a late PeerFound/completion can't reach the next
  // mode and stale peers don't linger. Runs on mount because mode starts local.
  useEffect(() => {
    if (mode !== "local") return;
    startDiscovery();
    return () => {
      discoveryGenRef.current++;
      setIsDiscovering(false);
      setPeers([]);
    };
  }, [mode]);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;

    const setupListener = async () => {
      unlisten = await listen<PlenumEventEnvelope>("plenum-event", (event) => {
        const { session_id, event: payload } = event.payload;
        // Drop events from a superseded transfer so they can't mutate this one.
        if (isStaleSession(session_id, activeSessionRef, sessionFloorRef)) return;
        if ("Discovery" in payload) {
           // Discovery events carry session_id 0 (they bypass the transfer gate),
           // so gate them on mode: a search from before a mode switch must not
           // repopulate peers while the user is in Internet mode.
           if (modeRef.current !== "local") return;
           const disc = payload.Discovery;
           if (typeof disc === "object" && "PeerFound" in disc) {
             setPeers((prev) => {
               const exists = prev.find(p => p.token === disc.PeerFound.token);
               if (exists) return prev;
               return [...prev, disc.PeerFound];
             });
           }
         } else if ("Transfer" in payload) {
           const trans: TransferEvent = payload.Transfer;
           if (terminalEventRef.current) {
             // Terminal state has precedence: ignore late events
             return;
           }
            if ("StateChanged" in trans) {
              if (trans.StateChanged.state !== "Closed") {
                setPhase("connecting");
                setTransferStatus(friendlyState(trans.StateChanged.state));
              }
            } else if ("Started" in trans) {
               if (autoResetRef.current) clearTimeout(autoResetRef.current);
               setPhase("transferring");
               setSendSuccess(false);
               setTransferStatus(trans.Started.resumed_bytes > 0
                 ? `Resuming ${trans.Started.file_name} from ${formatBytes(trans.Started.resumed_bytes)}...`
                 : `Sending ${trans.Started.file_name}...`);
               metricsRef.current = createTransferMetrics(trans.Started.total_bytes, trans.Started.resumed_bytes);
               setProgress({ transferred: trans.Started.resumed_bytes, total: trans.Started.total_bytes });
               setSpeedText(null);
               setEtaText(null);
               terminalEventRef.current = false;
            } else if ("ConnectionEstablished" in trans) {
               const connection = trans.ConnectionEstablished.mode === "Relay" ? "relay" : trans.ConnectionEstablished.mode === "Direct" ? "direct connection" : "local network";
               setTransferStatus(`Connected via ${connection}`);
            } else if ("AwaitingApproval" in trans) {
               setPhase("awaitingApproval");
               setTransferStatus("Waiting for receiver to accept...");
            } else if ("Resumed" in trans) {
               setPhase("transferring");
               setTransferStatus(`Resuming transfer from ${formatBytes(trans.Resumed.resumed_bytes)}...`);
               setProgress((current) => current ? { ...current, transferred: trans.Resumed.resumed_bytes } : current);
            } else if ("Progress" in trans) {
               setPhase("transferring");
               if (metricsRef.current) {
                 const metrics = updateTransferMetrics(metricsRef.current, trans.Progress.transferred_bytes);
                 setProgress({ transferred: trans.Progress.transferred_bytes, total: trans.Progress.total_bytes });
                 if (metrics.speedBps != null) {
                   setSpeedText(`${formatBytes(Math.round(metrics.speedBps))}/s`);
                 }
                 setEtaText(metrics.etaSeconds != null && metrics.etaSeconds > 0 ? `${metrics.etaSeconds}s left` : "");
               } else {
                 setProgress({ transferred: trans.Progress.transferred_bytes, total: trans.Progress.total_bytes });
               }
            } else if ("Declined" in trans) {
              terminalEventRef.current = true;
              setPhase("failed");
              setTransferStatus(trans.Declined.reason === "timeout" ? "Receiver did not respond" : "Receiver declined the transfer");
              setProgress(null);
              setSpeedText(null);
              setEtaText(null);
              metricsRef.current = null;
              if (autoResetRef.current) clearTimeout(autoResetRef.current);
              autoResetRef.current = setTimeout(() => {
                setPhase("idle");
                setTransferStatus("");
                terminalEventRef.current = false;
              }, 2000);
           } else if ("Failed" in trans) {
              terminalEventRef.current = true;
              setPhase("failed");
              setTransferStatus(trans.Failed.message);
              setProgress(null);
              setSpeedText(null);
              setEtaText(null);
              metricsRef.current = null;
           } else if ("Cancelled" in trans) {
              terminalEventRef.current = true;
              setPhase("cancelled");
              setTransferStatus("Transfer cancelled\nThe partial file can be resumed later.");
              setProgress(null);
              setSpeedText(null);
              setEtaText(null);
              metricsRef.current = null;
              if (autoResetRef.current) clearTimeout(autoResetRef.current);
              autoResetRef.current = setTimeout(() => {
                setPhase("idle");
                setTransferStatus("");
                terminalEventRef.current = false;
              }, 2000);
           } else if ("Completed" in trans) {
             terminalEventRef.current = true;
             setPhase("succeeded");
             const summary: TransferSummary = trans.Completed;
             const peerLabel = summary.peer_name ?? summary.peer ?? "device";
             const resumedBytes = summary.resumed_bytes ?? 0;
             const sessionBytes = Math.max(0, summary.total_bytes - resumedBytes);
             addHistoryEntry({
               direction: "send",
               fileName: summary.file_name,
               size: summary.total_bytes,
               resumedBytes,
               sessionBytes,
               peerName: peerLabel,
               durationMs: summary.elapsed_ms,
               mode: summary.mode,
               timestamp: new Date().toISOString(),
             });
             setSendSuccess(true);
             setRoomCodeInput("");
             setPinInput("");
             setTransferStatus(
               summary.elapsed_ms != null
                 ? (resumedBytes > 0
                     ? `Sent to ${peerLabel} in ${formatDuration(summary.elapsed_ms)} (Resumed from ${formatBytes(resumedBytes)})`
                     : `Sent to ${peerLabel} in ${formatDuration(summary.elapsed_ms)}`)
                 : `Sent ${summary.file_name} successfully!`
             );
             setProgress(null);
             setSpeedText(null);
             setEtaText(null);
             metricsRef.current = null;
             if (autoResetRef.current) clearTimeout(autoResetRef.current);
             autoResetRef.current = setTimeout(() => {
               setPhase("idle");
               setSendSuccess(false);
               setTransferStatus("");
               terminalEventRef.current = false;
             }, 5000);
            }
         } else if ("Log" in payload) {
           if (!terminalEventRef.current) {
             logToConsole(payload.Log);
           }
         }
      });

      const unlistenDrop = await listen<{ paths: string[] }>('tauri://drag-drop', (event) => {
        if (event.payload.paths.length > 0) {
          setSelectedPath(event.payload.paths[0]);
        }
        setIsDragging(false);
      });

      const unlistenDragEnter = await listen('tauri://drag-enter', () => setIsDragging(true));
      const unlistenDragLeave = await listen('tauri://drag-leave', () => setIsDragging(false));

      return () => {
        if (unlisten) unlisten();
        unlistenDrop();
        unlistenDragEnter();
        unlistenDragLeave();
      };
    };

    let cleanupFn: (() => void) | undefined;
    setupListener().then(cleanup => {
      // Unmounted before listen() resolved — cleanup below already ran with
      // cleanupFn still undefined, so tear the fresh listeners down now.
      if (cancelled) cleanup();
      else cleanupFn = cleanup;
    });

    return () => {
      cancelled = true;
      if (cleanupFn) cleanupFn();
      if (autoResetRef.current) clearTimeout(autoResetRef.current);
    };
  }, []);

  const handlePeerClick = (peer: DiscoverySummary) => {
    if (isTransferActive) return;
    if (!selectedPath) {
      setTransferStatus("Please select a file or folder first");
      return;
    }
    setPinInputPeer(peer);
    setPinInput("");
  };

  const handlePinSubmit = async () => {
    if (!pinInputPeer) return;
    if (isTransferActive) return;

    if (pinInputPeer.pin_required && pinInput.trim() === "") {
      setTransferStatus("Please enter the pairing code shown on the receiver's screen.");
      return;
    }

    if (!pinInputPeer.pin_required && pinInput.trim() !== "") {
      if (pinInput.trim().toUpperCase() !== pinInputPeer.token.toUpperCase()) {
        setTransferStatus("Error: Incorrect PIN entered.");
        return;
      }
    }

    setTransferStatus("Connecting to device...");
    terminalEventRef.current = false;
    setPhase("connecting");
    setIsTransferActive(true);
    const peer = pinInputPeer;
    setPinInputPeer(null);

    try {
      const req: SendRequest = {
        file_path: selectedPath!,
        address: peer.address,
        discovery_token: peer.pin_required ? pinInput.trim() : peer.token || undefined,
        device_name: settings.deviceName || undefined,
        permissions: { local_network: true, file_system_read: true, file_system_write: true, background_transfer: false },
        options: { chunk_size: 262144, window_size: 64, timeout_ticks: 15000 }
      };
      const result = await invoke<TransferSummary>("send_file_command", { request: req });
      console.log("Send completed:", result);
    } catch (err) {
      console.error("Send error:", err);
      if (!terminalEventRef.current) {
        terminalEventRef.current = true;
        setPhase("failed");
        setTransferStatus("Error: " + err);
      }
      setProgress(null);
    } finally {
      setIsTransferActive(false);
    }
  };

  const handleRoomCodeConnect = async () => {
    if (!selectedPath) {
      setTransferStatus("Please select a file or folder first");
      return;
    }
    const roomCode = roomCodeInput.trim().toUpperCase();
    if (roomCode === "") {
      setTransferStatus("Please enter a room code");
      return;
    }
    if (!/^[A-Z0-9]{9}$/.test(roomCode)) {
      setTransferStatus("Room codes are 9 letters or numbers");
      return;
    }
    if (isConnectingRemote || isTransferActive) return;

    setTransferStatus("Finding room…");
    terminalEventRef.current = false;
    setPhase("connecting");
    setIsConnectingRemote(true);
    setIsTransferActive(true);

    try {
      const myPeerId = await invoke<string>("generate_peer_id_command");
      const iceServers: IceServer[] = [...DEFAULT_ICE_SERVERS];
      const turn = await invoke<IceServer | null>("fetch_turn_credentials_command", {
        relayServerUrl: RELAY_SERVER_URL,
        peerId: myPeerId,
      });
      if (turn) iceServers.push(turn);
      const lookup = await lookupRoomWithGracePeriod(RELAY_SERVER_URL, roomCode, undefined, 4000, 500);
      if (lookup.status !== "exists") {
        throw new Error(roomLookupMessage(lookup));
      }
      setTransferStatus("Room found. Connecting…");
      const req: SendRemoteRequest = {
        file_path: selectedPath!,
        relay_server_url: RELAY_SERVER_URL,
        session_id: roomCode,
        my_peer_id: myPeerId,
        ice_servers: iceServers,
        connect_timeout_secs: 30,
        device_name: settings.deviceName || undefined,
        permissions: { local_network: true, file_system_read: true, file_system_write: true, background_transfer: false },
        options: { chunk_size: 32768, window_size: 128, timeout_ticks: 1000 }
      };
      const result = await invoke<TransferSummary>("send_file_remote_command", { request: req });
      console.log("Send completed:", result);
    } catch (err) {
      console.error("Send error:", err);
      if (!terminalEventRef.current) {
        terminalEventRef.current = true;
        setPhase("failed");
        setTransferStatus(err instanceof Error ? err.message : "Error: " + err);
      }
      setProgress(null);
    } finally {
      setIsConnectingRemote(false);
      setIsTransferActive(false);
    }
  };

  const handleModeChange = async (nextMode: "local" | "internet") => {
    if (nextMode === mode) return;
    // Cancel any in-flight send in the Rust backend before swapping modes.
    if (isTransferActive || isConnectingRemote) {
      terminalEventRef.current = true;
      try {
        await invoke("cancel_session_command", { sessionId: activeSessionRef.current });
      } catch (err) {
        console.error("Cancel on mode switch failed:", err);
      }
    }
    abandonSession(activeSessionRef, sessionFloorRef);
    resetSendUi();
    setMode(nextMode);
  };

  const resetSendUi = () => {
    if (autoResetRef.current) {
      clearTimeout(autoResetRef.current);
      autoResetRef.current = null;
    }
    setPhase("idle");
    setSendSuccess(false);
    setTransferStatus("");
    setProgress(null);
    setSpeedText(null);
    setEtaText(null);
    setIsTransferActive(false);
    setIsConnectingRemote(false);
    setPinInputPeer(null);
    setPinInput("");
    setRoomCodeInput("");
    metricsRef.current = null;
    terminalEventRef.current = false;
  };

  const handleCancelTransfer = async () => {
    if (phase === "cancelling" || terminalEventRef.current) return;
    setPhase("cancelling");
    setTransferStatus("Cancelling transfer...");
    try {
      await invoke("cancel_session_command", { sessionId: activeSessionRef.current });
    } catch (err) {
      console.error("Cancel failed:", err);
    }
  };

  const handleSelectFile = async () => {
    try {
      const selected = await open({ multiple: false, directory: false });
      if (selected && !Array.isArray(selected)) {
        setSelectedPath(selected);
      }
    } catch (err) {
      console.error(err);
    }
  };

  const isSuccess = phase === "succeeded" || sendSuccess;
  const isInFlight = (phase === "connecting" || phase === "awaitingApproval" || phase === "transferring" || phase === "cancelling" || isTransferActive || isConnectingRemote) && !isSuccess;

  return (
    <div style={{ position: "relative", height: "100%" }} data-phase={phase}>
      {isDragging && (
        <div style={{
          position: "absolute",
          top: 0, left: 0, right: 0, bottom: 0,
          backgroundColor: "rgba(0, 0, 0, 0.7)",
          zIndex: 100,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          borderRadius: "12px",
          border: "2px dashed var(--accent-primary)"
        }}>
          <h2 style={{ color: "var(--accent-primary)" }}>Drop file here to send</h2>
        </div>
      )}
      <div className="card-section">
        <h2 className="section-title">Mode</h2>
        <div className="card-grid">
          <div className="action-card" onClick={() => handleModeChange("local")} style={{ borderColor: mode === "local" ? "var(--accent-primary)" : "var(--border-color)" }}>
            <Wifi size={28} />
            <span>Local Network</span>
          </div>
          <div className="action-card" onClick={() => handleModeChange("internet")} style={{ borderColor: mode === "internet" ? "var(--accent-primary)" : "var(--border-color)" }}>
            <Globe size={28} />
            <span>Internet</span>
          </div>
        </div>
      </div>

      <div className="card-section">
        <h2 className="section-title">File</h2>
        <div className="action-card" onClick={handleSelectFile} style={{ borderColor: selectedPath ? "var(--accent-primary)" : "var(--border-color)", padding: "32px" }}>
          <File size={28} />
          <span>{selectedPath ? selectedPath.split(/[/\\]/).pop() : "Select a file to send"}</span>
        </div>
        <div style={{ marginTop: "8px", fontSize: "12px", color: "var(--text-secondary)", textAlign: "center" }}>
          or drag &amp; drop a file anywhere
        </div>
      </div>

      {mode === "internet" && (
        <div className="card-section">
          <div className="section-title">
            <span>Connect via room code</span>
          </div>
          <div style={{ display: "flex", gap: "12px" }}>
            <input
              type="text"
              value={roomCodeInput}
              onChange={(e) => setRoomCodeInput(e.target.value)}
              placeholder="Enter room code"
              style={{ flex: 1, padding: "10px 12px", borderRadius: "8px", border: "1px solid var(--border-color)", backgroundColor: "var(--bg-sidebar)", color: "var(--text-primary)", outline: "none", fontSize: "14px", letterSpacing: "2px", textTransform: "uppercase" }}
              onKeyDown={(e) => { if (e.key === "Enter") handleRoomCodeConnect(); }}
            />
            <button
              onClick={handleRoomCodeConnect}
              disabled={isConnectingRemote || isTransferActive}
              style={{ padding: "10px 20px", borderRadius: "8px", border: "none", backgroundColor: "var(--accent-primary)", color: "white", fontWeight: 600, cursor: (isConnectingRemote || isTransferActive) ? "default" : "pointer", opacity: (isConnectingRemote || isTransferActive) ? 0.6 : 1 }}
            >
              Connect
            </button>
          </div>

          {transferStatus && (
            <div style={{ marginTop: "24px", padding: "16px", backgroundColor: "var(--bg-card)", borderRadius: "8px", textAlign: "center", border: isSuccess ? "1px solid var(--accent-primary)" : "none" }}>
              <div style={{ display: "flex", alignItems: "center", justifyContent: "center", gap: "8px", fontSize: "14px", color: isSuccess ? "var(--text-primary)" : "var(--text-secondary)", fontWeight: isSuccess ? 600 : 400 }}>
                {isSuccess && <CheckCircle2 size={18} color="var(--accent-primary)" />}
                {transferStatus}
              </div>
              {progress && (
                <div style={{ marginTop: "12px", width: "100%" }}>
                  <div style={{ width: "100%", backgroundColor: "var(--bg-sidebar)", height: "6px", borderRadius: "3px", overflow: "hidden" }}>
                    <div style={{ width: `${progressPercent(progress.transferred, progress.total)}%`, backgroundColor: "var(--accent-primary)", height: "100%" }} />
                  </div>
                  <div style={{ display: "flex", justifyContent: "space-between", marginTop: "8px", fontSize: "12px", color: "var(--text-secondary)" }}>
                    <span>{Math.round(progressPercent(progress.transferred, progress.total))}%  •  {formatBytes(progress.transferred)} / {formatBytes(progress.total)}</span>
                    {speedText && <span>{speedText}{etaText ? ` • ${etaText}` : ""}</span>}
                  </div>
                </div>
              )}
              {isInFlight && (
                <div style={{ marginTop: "12px" }}>
                  <button
                    onClick={handleCancelTransfer}
                    disabled={phase === "cancelling"}
                    style={{
                      padding: "6px 14px",
                      borderRadius: "6px",
                      border: "1px solid var(--border-color)",
                      backgroundColor: "transparent",
                      color: phase === "cancelling" ? "var(--text-secondary)" : "var(--accent-primary)",
                      fontSize: "12px",
                      cursor: phase === "cancelling" ? "not-allowed" : "pointer",
                      opacity: phase === "cancelling" ? 0.6 : 1,
                    }}
                  >
                    {phase === "cancelling" ? "Cancelling..." : "Cancel transfer"}
                  </button>
                </div>
              )}
            </div>
          )}

          <div style={{ marginTop: "40px", textAlign: "center" }}>
            <p style={{ fontSize: "13px", color: "var(--text-secondary)", marginTop: "16px" }}>
              Ask the receiver for their room code, then click Connect to send over the internet.
            </p>
          </div>
        </div>
      )}

      {mode === "local" && (
      <div className="card-section">
        <div className="section-title" style={{ justifyContent: "space-between" }}>
          <div style={{ display: "flex", alignItems: "center", gap: "12px" }}>
            <span>Nearby devices</span>
            <RefreshCcw
              size={16}
              color={isDiscovering ? "var(--text-secondary)" : "var(--accent-primary)"}
              style={{ cursor: "pointer", opacity: isDiscovering ? 0.5 : 1 }}
              onClick={startDiscovery}
            />
          </div>
        </div>
        {peers.length === 0 && !isDiscovering && (
           <div style={{ padding: "24px", textAlign: "center", color: "var(--text-secondary)", fontSize: "14px" }}>
             Ready to send files. Make sure the receiver is open on the other device.
           </div>
        )}
        
        {peers.length === 0 && isDiscovering && (
           <div style={{ padding: "24px", textAlign: "center", color: "var(--text-secondary)", fontSize: "14px" }}>
             Searching for nearby devices...
           </div>
        )}

        {peers.map((peer, i) => (
          <div key={i} style={{ marginTop: "16px" }}>
            <div onClick={() => handlePeerClick(peer)} style={{
              backgroundColor: "var(--bg-card)",
              padding: "24px",
              borderRadius: pinInputPeer?.address === peer.address ? "12px 12px 0 0" : "12px",
              display: "flex",
              alignItems: "center",
              gap: "16px",
              cursor: isTransferActive ? "not-allowed" : "pointer",
              opacity: isTransferActive ? 0.5 : 1,
              pointerEvents: isTransferActive ? "none" : "auto",
              border: "1px solid transparent",
              borderBottom: pinInputPeer?.address === peer.address ? "1px solid var(--border-color)" : "1px solid transparent",
              transition: "all 0.2s ease"
            }}
            onMouseEnter={(e) => { if (!isTransferActive) e.currentTarget.style.borderColor = "var(--accent-primary)"; }}
            onMouseLeave={(e) => { if (pinInputPeer?.address !== peer.address) e.currentTarget.style.borderColor = "transparent"; }}
            >
              <Monitor size={40} color="var(--accent-primary)" />
              <div style={{ display: "flex", flexDirection: "column", gap: "4px" }}>
                <div style={{ fontWeight: 600, color: "var(--text-primary)" }}>{peer.hostname}</div>
                <div style={{ fontSize: "12px", color: "var(--text-secondary)" }}>{peer.address}</div>
              </div>
            </div>
            
            {pinInputPeer?.address === peer.address && (
              <div style={{ backgroundColor: "var(--bg-card)", padding: "16px 24px", borderRadius: "0 0 12px 12px", display: "flex", flexDirection: "column", gap: "12px" }}>
                <div style={{ fontSize: "13px", color: "var(--text-secondary)" }}>
                  Enter PIN if required, otherwise leave blank:
                </div>
                <div style={{ display: "flex", gap: "12px" }}>
                  <input 
                    type="text" 
                    value={pinInput} 
                    onChange={(e) => setPinInput(e.target.value)} 
                    placeholder="PIN" 
                    maxLength={6}
                    style={{ flex: 1, padding: "10px 12px", borderRadius: "8px", border: "1px solid var(--border-color)", backgroundColor: "var(--bg-sidebar)", color: "var(--text-primary)", outline: "none", fontSize: "14px", letterSpacing: "2px", textTransform: "uppercase" }}
                    onKeyDown={(e) => { if (e.key === "Enter") handlePinSubmit(); }}
                  />
                  <button onClick={handlePinSubmit} style={{ padding: "10px 20px", borderRadius: "8px", border: "none", backgroundColor: "var(--accent-primary)", color: "white", fontWeight: 600, cursor: "pointer" }}>
                    Connect
                  </button>
                </div>
              </div>
            )}
          </div>
        ))}

        {transferStatus && (
          <div style={{ marginTop: "24px", padding: "16px", backgroundColor: "var(--bg-card)", borderRadius: "8px", textAlign: "center", border: isSuccess ? "1px solid var(--accent-primary)" : "none" }}>
            <div style={{ display: "flex", alignItems: "center", justifyContent: "center", gap: "8px", fontSize: "14px", color: isSuccess ? "var(--text-primary)" : "var(--text-secondary)", fontWeight: isSuccess ? 600 : 400 }}>
              {isSuccess && <CheckCircle2 size={18} color="var(--accent-primary)" />}
              {transferStatus}
            </div>
            {progress && (
              <div style={{ marginTop: "12px", width: "100%" }}>
                <div style={{ width: "100%", backgroundColor: "var(--bg-sidebar)", height: "6px", borderRadius: "3px", overflow: "hidden" }}>
                  <div style={{ width: `${progressPercent(progress.transferred, progress.total)}%`, backgroundColor: "var(--accent-primary)", height: "100%" }} />
                </div>
                <div style={{ display: "flex", justifyContent: "space-between", marginTop: "8px", fontSize: "12px", color: "var(--text-secondary)" }}>
                  <span>{Math.round(progressPercent(progress.transferred, progress.total))}%  •  {formatBytes(progress.transferred)} / {formatBytes(progress.total)}</span>
                  {speedText && <span>{speedText}{etaText ? ` • ${etaText}` : ""}</span>}
                </div>
              </div>
            )}
            {isInFlight && (
              <div style={{ marginTop: "12px" }}>
                <button
                  onClick={handleCancelTransfer}
                  disabled={phase === "cancelling"}
                  style={{
                    padding: "6px 14px",
                    borderRadius: "6px",
                    border: "1px solid var(--border-color)",
                    backgroundColor: "transparent",
                    color: phase === "cancelling" ? "var(--text-secondary)" : "var(--accent-primary)",
                    fontSize: "12px",
                    cursor: phase === "cancelling" ? "not-allowed" : "pointer",
                    opacity: phase === "cancelling" ? 0.6 : 1,
                  }}
                >
                  {phase === "cancelling" ? "Cancelling..." : "Cancel transfer"}
                </button>
              </div>
            )}
          </div>
        )}

        <div style={{ marginTop: "40px", textAlign: "center" }}>
          <p style={{ fontSize: "13px", color: "var(--text-secondary)", marginTop: "16px" }}>
            Please ensure that the desired target is also on the same Wi-Fi network.
          </p>
        </div>
      </div>
      )}
    </div>
  );
};

export default SendPage;
