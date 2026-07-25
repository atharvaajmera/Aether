import React, { useState, useEffect, useRef } from "react";
import { File, RefreshCcw, Monitor, Wifi, Globe, CheckCircle2 } from "lucide-react";
import { open } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import { PlenumEvent, DiscoverRequest, DiscoverySummary, SendRequest, SendRemoteRequest, TransferSummary, TransferEvent, IceServer } from "../types/rust";
import { addHistoryEntry } from "../services/history";
import { formatBytes, formatDuration } from "../utils/format";
import { RELAY_SERVER_URL, DEFAULT_ICE_SERVERS } from "../config";

const STATE_LABELS: Record<string, string> = {
  Discovering: "Searching for devices...",
  Listening: "Ready to send files",
  Connecting: "Connecting to device...",
  SignalingConnected: "Connecting to device...",
  NegotiatingIce: "Establishing connection...",
  Connected: "Connected to device...",
};

const friendlyState = (state: string): string => STATE_LABELS[state] ?? "Connecting to device...";

const SendPage: React.FC = () => {
  const [mode, setMode] = useState<"local" | "internet">("local");
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
  const [sendSuccess, setSendSuccess] = useState(false);
  const [speedText, setSpeedText] = useState<string | null>(null);
  const [etaText, setEtaText] = useState<string | null>(null);
  // Auto-reset timer so a late Completed can't race a fresh transfer's UI.
  const autoResetRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const transferStartRef = useRef<number | null>(null);

  const startDiscovery = async () => {
    setIsDiscovering(true);
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
      setIsDiscovering(false);
    }
  };

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;

    const setupListener = async () => {
      unlisten = await listen<PlenumEvent>("plenum-event", (event) => {
        const payload = event.payload;
        if ("Discovery" in payload) {
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
            if ("StateChanged" in trans) {
              if (trans.StateChanged.state !== "Closed") {
                setTransferStatus(friendlyState(trans.StateChanged.state));
              }
           } else if ("Started" in trans) {
              if (autoResetRef.current) clearTimeout(autoResetRef.current);
              setSendSuccess(false);
              setTransferStatus(`Sending ${trans.Started.file_name}...`);
              setProgress({ transferred: 0, total: trans.Started.total_bytes });
              transferStartRef.current = Date.now();
              setSpeedText(null);
              setEtaText(null);
           } else if ("Progress" in trans) {
              setProgress({ transferred: trans.Progress.transferred_bytes, total: trans.Progress.total_bytes });
              if (transferStartRef.current) {
                const elapsed = (Date.now() - transferStartRef.current) / 1000;
                if (elapsed > 0) {
                  const speedBps = trans.Progress.transferred_bytes / elapsed;
                  setSpeedText(`${formatBytes(Math.round(speedBps))}/s`);
                  const remaining = trans.Progress.total_bytes - trans.Progress.transferred_bytes;
                  const eta = speedBps > 0 ? Math.round(remaining / speedBps) : 0;
                  setEtaText(eta > 0 ? `${eta}s left` : "");
                }
              }
           } else if ("Declined" in trans) {
              setTransferStatus(trans.Declined.reason === "timeout" ? "Receiver did not respond" : "Receiver declined the transfer");
              setProgress(null);
              setSpeedText(null);
              setEtaText(null);
              transferStartRef.current = null;
           } else if ("Completed" in trans) {
             const summary: TransferSummary = trans.Completed;
             const peerLabel = summary.peer_name ?? summary.peer ?? "device";
             addHistoryEntry({
               direction: "send",
               fileName: summary.file_name,
               size: summary.total_bytes,
               peerName: peerLabel,
               durationMs: summary.elapsed_ms,
               mode: summary.mode,
               timestamp: new Date().toISOString(),
             });
             setSendSuccess(true);
              setTransferStatus(
                summary.elapsed_ms != null
                  ? `Sent to ${peerLabel} in ${formatDuration(summary.elapsed_ms)}`
                  : `Sent ${summary.file_name} successfully!`
              );
              setProgress(null);
              setSpeedText(null);
              setEtaText(null);
              transferStartRef.current = null;
             if (autoResetRef.current) clearTimeout(autoResetRef.current);
             autoResetRef.current = setTimeout(() => {
               setSendSuccess(false);
               setTransferStatus("");
             }, 4000);
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
      cleanupFn = cleanup;
    });
    startDiscovery();

    return () => {
      if (cleanupFn) cleanupFn();
      if (autoResetRef.current) clearTimeout(autoResetRef.current);
    };
  }, []);

  const handlePeerClick = (peer: DiscoverySummary) => {
    if (!selectedPath) {
      setTransferStatus("Please select a file or folder first");
      return;
    }
    setPinInputPeer(peer);
    setPinInput("");
  };

  const handlePinSubmit = async () => {
    if (!pinInputPeer) return;
    
    if (pinInput.trim() !== "") {
      if (pinInput.trim().toUpperCase() !== pinInputPeer.token.toUpperCase()) {
        setTransferStatus("Error: Incorrect PIN entered.");
        return;
      }
    }

    setTransferStatus("Connecting to device...");
    const peer = pinInputPeer;
    setPinInputPeer(null);
    
    try {
      const req: SendRequest = {
        file_path: selectedPath!,
        address: peer.address,
        discovery_token: peer.token,
        permissions: { local_network: true, file_system_read: true, file_system_write: true, background_transfer: false },
        options: { chunk_size: 32768, window_size: 128, timeout_ticks: 1000 }
      };
      const result = await invoke<TransferSummary>("send_file_command", { request: req });
      console.log("Send completed:", result);
    } catch (err) {
      console.error("Send error:", err);
      setTransferStatus("Error: " + err);
      setProgress(null);
    }
  };

  const handleRoomCodeConnect = async () => {
    if (!selectedPath) {
      setTransferStatus("Please select a file or folder first");
      return;
    }
    if (roomCodeInput.trim() === "") {
      setTransferStatus("Please enter a room code");
      return;
    }

    setTransferStatus("Connecting to relay...");
    setIsConnectingRemote(true);

    try {
      const myPeerId = await invoke<string>("generate_peer_id_command");
      const iceServers: IceServer[] = [...DEFAULT_ICE_SERVERS];
      const turn = await invoke<IceServer | null>("fetch_turn_credentials_command", {
        relayServerUrl: RELAY_SERVER_URL,
        peerId: myPeerId,
      });
      if (turn) iceServers.push(turn);
      const req: SendRemoteRequest = {
        file_path: selectedPath!,
        relay_server_url: RELAY_SERVER_URL,
        session_id: roomCodeInput.trim().toUpperCase(),
        my_peer_id: myPeerId,
        ice_servers: iceServers,
        connect_timeout_secs: 30,
        permissions: { local_network: true, file_system_read: true, file_system_write: true, background_transfer: false },
        options: { chunk_size: 32768, window_size: 128, timeout_ticks: 1000 }
      };
      const result = await invoke<TransferSummary>("send_file_remote_command", { request: req });
      console.log("Send completed:", result);
    } catch (err) {
      console.error("Send error:", err);
      setTransferStatus("Error: " + err);
      setProgress(null);
    } finally {
      setIsConnectingRemote(false);
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
  return (
    <div style={{ position: "relative", height: "100%" }}>
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
          <div className="action-card" onClick={() => setMode("local")} style={{ borderColor: mode === "local" ? "var(--accent-primary)" : "var(--border-color)" }}>
            <Wifi size={28} />
            <span>Local Network</span>
          </div>
          <div className="action-card" onClick={() => setMode("internet")} style={{ borderColor: mode === "internet" ? "var(--accent-primary)" : "var(--border-color)" }}>
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
              disabled={isConnectingRemote}
              style={{ padding: "10px 20px", borderRadius: "8px", border: "none", backgroundColor: "var(--accent-primary)", color: "white", fontWeight: 600, cursor: isConnectingRemote ? "default" : "pointer", opacity: isConnectingRemote ? 0.6 : 1 }}
            >
              Connect
            </button>
          </div>

          {transferStatus && (
            <div style={{ marginTop: "24px", padding: "16px", backgroundColor: "var(--bg-card)", borderRadius: "8px", textAlign: "center", border: sendSuccess ? "1px solid var(--accent-primary)" : "none" }}>
              <div style={{ display: "flex", alignItems: "center", justifyContent: "center", gap: "8px", fontSize: "14px", color: sendSuccess ? "var(--text-primary)" : "var(--text-secondary)", fontWeight: sendSuccess ? 600 : 400 }}>
                {sendSuccess && <CheckCircle2 size={18} color="var(--accent-primary)" />}
                {transferStatus}
              </div>
              {progress && (
                <div style={{ marginTop: "12px", width: "100%" }}>
                  <div style={{ width: "100%", backgroundColor: "var(--bg-sidebar)", height: "6px", borderRadius: "3px", overflow: "hidden" }}>
                    <div style={{ width: `${(progress.transferred / progress.total) * 100}%`, backgroundColor: "var(--accent-primary)", height: "100%" }} />
                  </div>
                  <div style={{ display: "flex", justifyContent: "space-between", marginTop: "8px", fontSize: "12px", color: "var(--text-secondary)" }}>
                    <span>{formatBytes(progress.transferred)} / {formatBytes(progress.total)}</span>
                    {speedText && <span>{speedText}{etaText ? ` • ${etaText}` : ""}</span>}
                  </div>
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
              cursor: "pointer",
              border: "1px solid transparent",
              borderBottom: pinInputPeer?.address === peer.address ? "1px solid var(--border-color)" : "1px solid transparent",
              transition: "all 0.2s ease"
            }}
            onMouseEnter={(e) => e.currentTarget.style.borderColor = "var(--accent-primary)"}
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
          <div style={{ marginTop: "24px", padding: "16px", backgroundColor: "var(--bg-card)", borderRadius: "8px", textAlign: "center", border: sendSuccess ? "1px solid var(--accent-primary)" : "none" }}>
            <div style={{ display: "flex", alignItems: "center", justifyContent: "center", gap: "8px", fontSize: "14px", color: sendSuccess ? "var(--text-primary)" : "var(--text-secondary)", fontWeight: sendSuccess ? 600 : 400 }}>
              {sendSuccess && <CheckCircle2 size={18} color="var(--accent-primary)" />}
              {transferStatus}
            </div>
            {progress && (
              <div style={{ marginTop: "12px", width: "100%" }}>
                <div style={{ width: "100%", backgroundColor: "var(--bg-sidebar)", height: "6px", borderRadius: "3px", overflow: "hidden" }}>
                  <div style={{ width: `${(progress.transferred / progress.total) * 100}%`, backgroundColor: "var(--accent-primary)", height: "100%" }} />
                </div>
                <div style={{ display: "flex", justifyContent: "space-between", marginTop: "8px", fontSize: "12px", color: "var(--text-secondary)" }}>
                  <span>{formatBytes(progress.transferred)} / {formatBytes(progress.total)}</span>
                  {speedText && <span>{speedText}{etaText ? ` • ${etaText}` : ""}</span>}
                </div>
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
