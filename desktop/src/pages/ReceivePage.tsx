import React, { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import { downloadDir } from "@tauri-apps/api/path";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { Copy, Check, Wifi, Globe, FolderOpen, CheckCircle2 } from "lucide-react";
import { PlenumEventEnvelope, TransferEvent, ReceiveRequest, ReceiveRemoteRequest, TransferSummary, IceServer } from "../types/rust";
import { useSettings } from "../context/SettingsContext";
import { addHistoryEntry } from "../services/history";
import { formatBytes, formatDuration } from "../utils/format";
import { isStaleSession, abandonSession } from "../utils/session";
import TransferAcceptDialog, { IncomingTransfer } from "../components/TransferAcceptDialog";
import { RELAY_SERVER_URL, DEFAULT_ICE_SERVERS } from "../config";

type LogEvent = { level: string; message: string };

const logToConsole = (_log: LogEvent) => undefined;

const STATE_LABELS: Record<string, string> = {
  Discovering: "Searching...",
  Listening: "Ready to receive files",
  Connecting: "Connecting to device...",
  SignalingConnected: "Connecting to device...",
  NegotiatingIce: "Establishing connection...",
  Connected: "Connected to device...",
};

const friendlyState = (state: string): string => STATE_LABELS[state] ?? "Connecting to device...";

const ReceivePage: React.FC = () => {
  const [mode, setMode] = useState<"local" | "internet">("local");
  const [deviceName, setDeviceName] = useState<string>("Loading...");
  const [localIp, setLocalIp] = useState<string>("");
  const [username, setUsername] = useState<string>("");
  const [status, setStatus] = useState<string>("Ready to receive files...");
  const [progress, setProgress] = useState<{ transferred: number, total: number } | null>(null);
  const [pin, setPin] = useState<string | null>(null);
  const [port, setPort] = useState<number | null>(null);
  const [copied, setCopied] = useState(false);
  const [roomCode, setRoomCode] = useState<string | null>(null);
  const [roomCodeCopied, setRoomCodeCopied] = useState(false);
  const [incoming, setIncoming] = useState<IncomingTransfer | null>(null);
  const [savedPath, setSavedPath] = useState<string | null>(null);
  const [speedText, setSpeedText] = useState<string | null>(null);
  const [etaText, setEtaText] = useState<string | null>(null);
  const [technicalDetails, setTechnicalDetails] = useState<string | null>(null);
  const [transferFailed, setTransferFailed] = useState(false);
  const transferStartRef = useRef<number | null>(null);
  const terminalEventRef = useRef(false);
  const activeSessionRef = useRef(0);
  const sessionFloorRef = useRef(0);
  const { settings } = useSettings();
  const outputDirRef = useRef<string>("");

  const handleCopyPin = () => {
    if (pin) {
      navigator.clipboard.writeText(pin);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  };

  const handleCopyRoomCode = () => {
    if (roomCode) {
      navigator.clipboard.writeText(roomCode);
      setRoomCodeCopied(true);
      setTimeout(() => setRoomCodeCopied(false), 2000);
    }
  };

  const handleAcceptResponse = (accept: boolean) => {
    setIncoming(null);
    invoke("respond_to_incoming_command", { accept }).catch(() => setStatus("Could not respond to the transfer request. Please try again."));
    if (!accept) setStatus("Transfer declined");
  };

  const handleOpenFolder = () => {
    if (savedPath) {
      revealItemInDir(savedPath).catch(() => setStatus("Could not open the download folder."));
    }
  };

  const handleModeChange = async (nextMode: "local" | "internet") => {
    if (nextMode === mode) return;
    setStatus("Switching receiver mode...");
    try {
      await invoke("cancel_session_command");
    } catch (err) {
      setStatus("Could not switch modes cleanly. Please try again.");
    }
    setMode(nextMode);
  };

  // Shared per-event handler for both local and internet receivers.
  const handleTransferEvent = (trans: TransferEvent) => {
    if ("StateChanged" in trans) {
      if (trans.StateChanged.state !== "Closed") {
        if (trans.StateChanged.state === "Listening") {
          setStatus("Ready to receive files");
        } else {
          setStatus(friendlyState(trans.StateChanged.state));
        }
      }
    } else if ("IncomingRequest" in trans) {
      const req = trans.IncomingRequest;
      if (!settingsRef.current.receive.autoAccept) {
        setIncoming({
          fileName: req.file_name,
          totalBytes: req.total_bytes,
          peer: req.peer,
          senderName: req.sender_name,
        });
        setStatus("Incoming file — waiting for your decision");
      }
    } else if ("ConnectionEstablished" in trans) {
      const connection = trans.ConnectionEstablished.mode === "Relay" ? "relay" : trans.ConnectionEstablished.mode === "Direct" ? "direct connection" : "local network";
      setStatus(`Connected via ${connection}`);
    } else if ("Cancelled" in trans) {
      terminalEventRef.current = true;
      setIncoming(null);
      setStatus("Transfer cancelled");
      setProgress(null);
      setSpeedText(null);
      setEtaText(null);
      transferStartRef.current = null;
    } else if ("Declined" in trans) {
      terminalEventRef.current = true;
      setIncoming(null);
      setStatus(trans.Declined.reason === "cancelled" ? "Sender cancelled the transfer" : "Transfer declined");
      setProgress(null);
      setSpeedText(null);
      setEtaText(null);
      transferStartRef.current = null;
    } else if ("Failed" in trans) {
      terminalEventRef.current = true;
      setTransferFailed(true);
      setStatus(trans.Failed.message);
      setProgress(null);
      setSpeedText(null);
      setEtaText(null);
      transferStartRef.current = null;
    } else if ("Started" in trans) {
      terminalEventRef.current = false;
      setTransferFailed(false);
      setTechnicalDetails(null);
      setIncoming(null);
      setSavedPath(null);
      setStatus(trans.Started.resumed_bytes > 0
        ? `Resuming ${trans.Started.file_name} from ${formatBytes(trans.Started.resumed_bytes)}...`
        : `Receiving ${trans.Started.file_name}...`);
      setProgress({ transferred: trans.Started.resumed_bytes, total: trans.Started.total_bytes });
      transferStartRef.current = Date.now();
      setSpeedText(null);
      setEtaText(null);
    } else if ("Resumed" in trans) {
      setStatus(`Resuming receive from ${formatBytes(trans.Resumed.resumed_bytes)}...`);
      setProgress((current) => current ? { ...current, transferred: trans.Resumed.resumed_bytes } : current);
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
    } else if ("Completed" in trans) {
      terminalEventRef.current = true;
      setTransferFailed(false);
      setTechnicalDetails(null);
      const summary: TransferSummary = trans.Completed;
      const path = outputDirRef.current
        ? `${outputDirRef.current}${outputDirRef.current.endsWith("\\") || outputDirRef.current.endsWith("/") ? "" : "\\"}${summary.file_name}`
        : null;
      addHistoryEntry({
        direction: "receive",
        fileName: summary.file_name,
        size: summary.total_bytes,
        peerName: summary.peer_name ?? summary.peer ?? "Unknown sender",
        durationMs: summary.elapsed_ms,
        mode: summary.mode,
        path: path ?? undefined,
        timestamp: new Date().toISOString(),
      });
      setStatus(
        summary.elapsed_ms != null
          ? `Received ${summary.file_name} in ${formatDuration(summary.elapsed_ms)}`
          : `Received ${summary.file_name} successfully!`
      );
      setSavedPath(path);
      setProgress(null);
      setSpeedText(null);
      setEtaText(null);
      transferStartRef.current = null;
    }
  };

  // Keep latest settings visible to the long-lived listen callback.
  const settingsRef = useRef(settings);
  useEffect(() => { settingsRef.current = settings; }, [settings]);
  const handleTransferEventRef = useRef(handleTransferEvent);
  handleTransferEventRef.current = handleTransferEvent;
  const handleLogEvent = (log: LogEvent) => {
    logToConsole(log);
    if (log.level === "Error") {
      setTechnicalDetails(log.message);
    }
  };
  const handleLogEventRef = useRef(handleLogEvent);
  handleLogEventRef.current = handleLogEvent;

  useEffect(() => {
    invoke<string>("get_device_name").then(setDeviceName).catch(console.error);
    invoke<string>("get_local_ip").then(setLocalIp).catch(console.error);
    invoke<string>("get_username").then(setUsername).catch(console.error);
  }, []);

  useEffect(() => {
    if (mode !== "local") return;

    let unlisten: UnlistenFn | undefined;
    let cancelled = false;

    const setupReceiver = async () => {
      // 1. Listen for events
      const fn = await listen<PlenumEventEnvelope>("plenum-event", (event) => {
        const { session_id, event: payload } = event.payload;
        // Drop events from a superseded session so they can't mutate this one.
        if (isStaleSession(session_id, activeSessionRef, sessionFloorRef)) return;
        if ("Discovery" in payload) {
           const disc = payload.Discovery;
           if (typeof disc === "object" && "BroadcastStarted" in disc) {
             setPort(disc.BroadcastStarted.port);
             if (settingsRef.current.receive.requirePin) {
               setPin(disc.BroadcastStarted.token);
             }
           }
         } else if ("Transfer" in payload) {
           handleTransferEventRef.current(payload.Transfer);
         } else if ("Log" in payload) {
           handleLogEventRef.current(payload.Log);
         }
      });
      // Unmounted before listen() resolved — drop the listener immediately.
      if (cancelled) { fn(); return; }
      unlisten = fn;

      // 2. Resolve the real system Downloads directory
      const downloadsPath = await downloadDir();
      outputDirRef.current = downloadsPath;

      const req: ReceiveRequest = {
        port: 0, // auto-assign; firewall allows the whole exe
        output_dir: downloadsPath,
        announce_on_lan: true,
        require_pin: settings.receive.requirePin,
        // Auto-accept skips the accept dialog; otherwise it gates.
        auto_accept: settings.receive.autoAccept,
        device_name: settings.deviceName || undefined,
        permissions: { local_network: true, file_system_read: true, file_system_write: true, background_transfer: false },
        options: { chunk_size: 262144, window_size: 64, timeout_ticks: 15000 }
      };

      while (!cancelled) {
        terminalEventRef.current = false;
        try {
          const result = await invoke<TransferSummary>("receive_file_command", { request: req });
          void result;
          if (!cancelled) await new Promise(resolve => setTimeout(resolve, 1500));
        } catch (err) {
          if (!cancelled && !terminalEventRef.current) setStatus(`Could not receive the file: ${err instanceof Error ? err.message : String(err)}`);
          break;
        }
      }
    };

    setupReceiver();

    return () => {
      cancelled = true;
      // Abandon this session so a trailing event with same id can't hit the next mode.
      abandonSession(activeSessionRef, sessionFloorRef);
      if (unlisten) unlisten();
    };
  }, [mode]);

  useEffect(() => {
    if (mode !== "internet") return;

    setStatus("Generating room code...");
    setRoomCode(null);
    let cancelled = false;
    let unlisten: UnlistenFn | undefined;

    const setupRemoteReceiver = async () => {
      const fn = await listen<PlenumEventEnvelope>("plenum-event", (event) => {
        const { session_id, event: payload } = event.payload;
        // Drop events from a superseded session so they can't mutate this one.
        if (isStaleSession(session_id, activeSessionRef, sessionFloorRef)) return;
        if ("Transfer" in payload) {
          handleTransferEventRef.current(payload.Transfer);
        } else if ("Log" in payload) {
          handleLogEventRef.current(payload.Log);
        }
      });
      // Unmounted before listen() resolved — drop the listener immediately.
      if (cancelled) { fn(); return; }
      unlisten = fn;

      const [code, myPeerId] = await Promise.all([
        invoke<string>("generate_room_code_command"),
        invoke<string>("generate_peer_id_command"),
      ]);

      if (cancelled) return;
      setRoomCode(code);

      setStatus("Waiting for sender...");

      const downloadsPath = await downloadDir();
      outputDirRef.current = downloadsPath;

      const iceServers: IceServer[] = [...DEFAULT_ICE_SERVERS];
      const turn = await invoke<IceServer | null>("fetch_turn_credentials_command", {
        relayServerUrl: RELAY_SERVER_URL,
        peerId: myPeerId,
      });
      if (turn) iceServers.push(turn);

      const req: ReceiveRemoteRequest = {
        output_dir: downloadsPath,
        relay_server_url: RELAY_SERVER_URL,
        session_id: code,
        my_peer_id: myPeerId,
        ice_servers: iceServers,
        connect_timeout_secs: 30,
        auto_accept: settings.receive.autoAccept,
        device_name: settings.deviceName || undefined,
        permissions: { local_network: true, file_system_read: true, file_system_write: true, background_transfer: false },
        options: { chunk_size: 32768, window_size: 128, timeout_ticks: 1000 }
      };

      try {
        const result = await invoke<TransferSummary>("receive_file_remote_command", { request: req });
        void result;
      } catch (err) {
        if (!cancelled && !terminalEventRef.current) setStatus(`Could not connect: ${err instanceof Error ? err.message : String(err)}`);
      }
    };

    setupRemoteReceiver();

    return () => {
      cancelled = true;
      // Abandon this session so a trailing event with same id can't hit the next mode.
      abandonSession(activeSessionRef, sessionFloorRef);
      if (unlisten) unlisten();
    };
  }, [mode]);

  useEffect(() => () => {
    invoke("cancel_session_command").catch(console.error);
  }, []);

  return (
    <div className="receive-container">
      {incoming && <TransferAcceptDialog incoming={incoming} onRespond={handleAcceptResponse} />}

      <div className="card-grid" style={{ width: "100%", maxWidth: "300px", marginBottom: "24px" }}>
        <div className="action-card" onClick={() => handleModeChange("local")} style={{ borderColor: mode === "local" ? "var(--accent-primary)" : "var(--border-color)" }}>
          <Wifi size={24} />
          <span>Local Network</span>
        </div>
        <div className="action-card" onClick={() => handleModeChange("internet")} style={{ borderColor: mode === "internet" ? "var(--accent-primary)" : "var(--border-color)" }}>
          <Globe size={24} />
          <span>Internet</span>
        </div>
      </div>

      <div className="ring-wrapper">
        <div className="segmented-ring"></div>
        <div className="core-circle"></div>
      </div>

      <div style={{ display: "flex", flexDirection: "column", alignItems: "center", marginTop: "20px" }}>
        {mode === "local" && (
          <>
            <h1 className="device-name" style={{ textAlign: "center" }}>{settings.deviceName || deviceName}</h1>
            <div className="device-id" style={{ textAlign: "center", userSelect: "text" }}>
              {localIp}{port ? `:${port}` : ''} {username ? `• ${username}` : ''}
            </div>
          </>
        )}

        {mode === "internet" && (
          <h1 className="device-name" style={{ textAlign: "center" }}>{settings.deviceName || deviceName}</h1>
        )}

        <div style={{ marginTop: "20px", fontSize: "14px", color: "var(--text-secondary)", textAlign: "center" }}>
          {status}
        </div>

        {import.meta.env.DEV && transferFailed && technicalDetails && (
          <details className="technical-details">
            <summary>Technical details</summary>
            <p>{technicalDetails}</p>
          </details>
        )}

        {mode === "local" && pin && (
          <div style={{ marginTop: "16px", padding: "12px 24px", backgroundColor: "var(--bg-card)", borderRadius: "8px", border: "1px dashed var(--accent-primary)", display: "flex", flexDirection: "column", alignItems: "center" }}>
            <div style={{ fontSize: "12px", color: "var(--text-secondary)", textAlign: "center", marginBottom: "4px" }}>PIN Required</div>
            <div style={{ display: "flex", alignItems: "center", gap: "12px" }}>
              <div style={{ fontSize: "24px", fontWeight: "bold", color: "var(--accent-primary)", letterSpacing: "4px" }}>{pin}</div>
              <div onClick={handleCopyPin} style={{ cursor: "pointer", padding: "4px", backgroundColor: "var(--bg-sidebar)", borderRadius: "4px" }}>
                {copied ? <Check size={16} color="var(--accent-primary)" /> : <Copy size={16} color="var(--text-secondary)" />}
              </div>
            </div>
          </div>
        )}

        {mode === "internet" && roomCode && (
          <div style={{ marginTop: "16px", padding: "12px 24px", backgroundColor: "var(--bg-card)", borderRadius: "8px", border: "1px dashed var(--accent-primary)", display: "flex", flexDirection: "column", alignItems: "center" }}>
            <div style={{ fontSize: "12px", color: "var(--text-secondary)", textAlign: "center", marginBottom: "4px" }}>Room Code</div>
            <div style={{ display: "flex", alignItems: "center", gap: "12px" }}>
              <div style={{ fontSize: "24px", fontWeight: "bold", color: "var(--accent-primary)", letterSpacing: "4px" }}>{roomCode}</div>
              <div onClick={handleCopyRoomCode} style={{ cursor: "pointer", padding: "4px", backgroundColor: "var(--bg-sidebar)", borderRadius: "4px" }}>
                {roomCodeCopied ? <Check size={16} color="var(--accent-primary)" /> : <Copy size={16} color="var(--text-secondary)" />}
              </div>
            </div>
          </div>
        )}

        {progress && (
          <div style={{ marginTop: "16px", width: "80%", maxWidth: "300px" }}>
            <div style={{ width: "100%", backgroundColor: "var(--bg-sidebar)", height: "8px", borderRadius: "4px", overflow: "hidden" }}>
              <div style={{ width: `${(progress.transferred / progress.total) * 100}%`, backgroundColor: "var(--accent-primary)", height: "100%" }} />
            </div>
            <div style={{ fontSize: "12px", color: "var(--text-secondary)", marginTop: "8px", textAlign: "center" }}>
              <div style={{ display: "flex", justifyContent: "space-between" }}>
                <span>{Math.round((progress.transferred / progress.total) * 100)}%  •  {formatBytes(progress.transferred)} / {formatBytes(progress.total)}</span>
                {speedText && <span>{speedText}{etaText ? ` • ${etaText}` : ""}</span>}
              </div>
            </div>
          </div>
        )}

        {savedPath && (
          <div style={{ marginTop: "16px", padding: "12px 20px", backgroundColor: "var(--bg-card)", borderRadius: "8px", border: "1px solid var(--accent-primary)", display: "flex", flexDirection: "column", alignItems: "center", gap: "8px", maxWidth: "360px" }}>
            <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
              <CheckCircle2 size={18} color="var(--accent-primary)" />
              <div style={{ fontSize: "13px", color: "var(--text-primary)", fontWeight: 600, wordBreak: "break-all" }}>
                Saved to {savedPath}
              </div>
            </div>
            <button
              onClick={handleOpenFolder}
              style={{ display: "flex", alignItems: "center", gap: "6px", padding: "8px 16px", borderRadius: "8px", border: "none", backgroundColor: "var(--accent-primary)", color: "white", fontWeight: 600, cursor: "pointer", fontSize: "13px" }}
            >
              <FolderOpen size={16} />
              Open folder
            </button>
          </div>
        )}
      </div>


    </div>
  );
};

export default ReceivePage;
