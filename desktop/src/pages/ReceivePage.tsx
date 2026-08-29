import React, { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import { downloadDir } from "@tauri-apps/api/path";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { Copy, Check, Wifi, Globe, FolderOpen, CheckCircle2 } from "lucide-react";
import { PlenumEventEnvelope, TransferEvent, ReceiveRequest, ReceiveRemoteRequest, TransferSummary, IceServer, TransferUiPhase } from "../types/rust";
import { useSettings } from "../context/SettingsContext";
import { addHistoryEntry } from "../services/history";
import { formatBytes, formatDuration, progressPercent } from "../utils/format";
import { isStaleSession, abandonSession } from "../utils/session";
import { createTransferMetrics, updateTransferMetrics, TransferMetricsState } from "../utils/transferMetrics";
import { waitForRoomRegistration, roomLookupMessage } from "../utils/relayUrl";
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
  const [phase, setPhase] = useState<TransferUiPhase>("idle");
  const [deviceName, setDeviceName] = useState<string | null>("Loading...");
  const [localIp, setLocalIp] = useState<string | null>(null);
  const [username, setUsername] = useState<string | null>(null);
  const [status, setStatus] = useState<string>("Ready to receive files...");
  const [progress, setProgress] = useState<{ transferred: number, total: number } | null>(null);
  const [pin, setPin] = useState<string | null>(null);
  const [port, setPort] = useState<number | null>(null);
  const [copied, setCopied] = useState(false);
  const [roomCode, setRoomCode] = useState<string | null>(null);
  const [roomCodeCopied, setRoomCodeCopied] = useState(false);
  // Set when a clipboard write is denied so the UI can prompt manual copy.
  const [copyError, setCopyError] = useState<string | null>(null);
  const [incoming, setIncoming] = useState<IncomingTransfer | null>(null);
  const [savedPath, setSavedPath] = useState<string | null>(null);
  const [speedText, setSpeedText] = useState<string | null>(null);
  const [etaText, setEtaText] = useState<string | null>(null);
  const [technicalDetails, setTechnicalDetails] = useState<string | null>(null);
  const [transferFailed, setTransferFailed] = useState(false);
  const successResetRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const metricsRef = useRef<TransferMetricsState | null>(null);
  const terminalEventRef = useRef(false);
  const activeSessionRef = useRef(0);
  const sessionFloorRef = useRef(0);
  // Resolves when the previous receiver session has fully finished in the
  // backend. React cleanup callbacks can't be async, so instead of awaiting
  // teardown in the destructor (impossible), the *next* session waits on this
  // barrier before starting — preventing a new session from binding a port or
  // relay room the outgoing one hasn't released yet.
  const teardownRef = useRef<Promise<void>>(Promise.resolve());
  const { settings } = useSettings();
  const outputDirRef = useRef<string>("");

  const handleCopyPin = async () => {
    if (!pin) return;
    try {
      await navigator.clipboard.writeText(pin);
      setCopyError(null);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch (err) {
      // Clipboard access can be denied; don't show a false "copied" state.
      console.error("Clipboard write failed:", err);
      setCopyError("Couldn't copy to clipboard — select and copy the code manually.");
    }
  };

  const handleCopyRoomCode = async () => {
    if (!roomCode) return;
    try {
      await navigator.clipboard.writeText(roomCode);
      setCopyError(null);
      setRoomCodeCopied(true);
      setTimeout(() => setRoomCodeCopied(false), 2000);
    } catch (err) {
      console.error("Clipboard write failed:", err);
      setCopyError("Couldn't copy to clipboard — select and copy the code manually.");
    }
  };

  const handleAcceptResponse = (accept: boolean) => {
    const sessionId = activeSessionRef.current;
    setIncoming(null);
    invoke("respond_to_incoming_command", { sessionId, accept }).catch(() => setStatus("Could not respond to the transfer request. Please try again."));
    if (!accept) {
      terminalEventRef.current = true;
      setPhase("failed");
      setStatus("Transfer declined");
    }
  };

  const handleOpenFolder = () => {
    if (savedPath) {
      revealItemInDir(savedPath).catch(() => setStatus("Could not open the download folder."));
    }
  };

  const resetReceiveUi = () => {
    if (successResetRef.current) {
      clearTimeout(successResetRef.current);
      successResetRef.current = null;
    }
    setSavedPath(null);
    setProgress(null);
    setSpeedText(null);
    setEtaText(null);
    setPhase("listening");
    setStatus("Ready to receive files");
    terminalEventRef.current = false;
    metricsRef.current = null;
  };

  const handleModeChange = async (nextMode: "local" | "internet") => {
    if (nextMode === mode) return;
    if (successResetRef.current) {
      clearTimeout(successResetRef.current);
      successResetRef.current = null;
    }
    setStatus("Switching receiver mode...");
    setPhase("idle");
    try {
      await invoke("cancel_session_command", { sessionId: activeSessionRef.current });
    } catch (err) {
      setStatus("Could not switch modes cleanly. Please try again.");
    }
    setMode(nextMode);
  };

  // Shared per-event handler for both local and internet receivers.
  const handleTransferEvent = (trans: TransferEvent) => {
    if (terminalEventRef.current) {
      // Terminal state has precedence: ignore late events
      return;
    }
    if ("StateChanged" in trans) {
      if (trans.StateChanged.state !== "Closed") {
        if (trans.StateChanged.state === "Listening") {
          setPhase("listening");
          setStatus("Ready to receive files");
        } else {
          setPhase("connecting");
          setStatus(friendlyState(trans.StateChanged.state));
        }
      }
    } else if ("IncomingRequest" in trans) {
      const req = trans.IncomingRequest;
      if (!settingsRef.current.receive.autoAccept) {
        setIncoming({
          fileName: req.file_name,
          totalBytes: req.total_bytes,
          senderName: req.sender_name ?? req.peer ?? "Unknown device",
          peer: req.peer,
        });
        setPhase("awaitingApproval");
        setStatus(`Incoming file: ${req.file_name} from ${req.sender_name ?? req.peer ?? "device"}`);
      }
    } else if ("ConnectionEstablished" in trans) {
      const connection = trans.ConnectionEstablished.mode === "Relay" ? "relay" : trans.ConnectionEstablished.mode === "Direct" ? "direct connection" : "local network";
      setStatus(`Connected via ${connection}`);
    } else if ("Cancelled" in trans) {
      terminalEventRef.current = true;
      setPhase("cancelled");
      setIncoming(null);
      setStatus("Sender cancelled the transfer");
      setProgress(null);
      setSpeedText(null);
      setEtaText(null);
      metricsRef.current = null;
      if (successResetRef.current) clearTimeout(successResetRef.current);
      successResetRef.current = setTimeout(() => {
        resetReceiveUi();
      }, 2000);
    } else if ("Declined" in trans) {
      terminalEventRef.current = true;
      setPhase("failed");
      setIncoming(null);
      setStatus(trans.Declined.reason === "cancelled" ? "Sender cancelled the transfer" : "Transfer declined");
      setProgress(null);
      setSpeedText(null);
      setEtaText(null);
      metricsRef.current = null;
      if (successResetRef.current) clearTimeout(successResetRef.current);
      successResetRef.current = setTimeout(() => {
        resetReceiveUi();
      }, 2000);
    } else if ("Failed" in trans) {
      terminalEventRef.current = true;
      setPhase("failed");
      setTransferFailed(true);
      setStatus(trans.Failed.message);
      setProgress(null);
      setSpeedText(null);
      setEtaText(null);
      metricsRef.current = null;
    } else if ("Started" in trans) {
      if (successResetRef.current) {
        clearTimeout(successResetRef.current);
        successResetRef.current = null;
      }
      terminalEventRef.current = false;
      setPhase("transferring");
      setTransferFailed(false);
      setTechnicalDetails(null);
      setIncoming(null);
      setSavedPath(null);
      setStatus(trans.Started.resumed_bytes > 0
        ? `Resuming ${trans.Started.file_name} from ${formatBytes(trans.Started.resumed_bytes)}...`
        : `Receiving ${trans.Started.file_name}...`);
      metricsRef.current = createTransferMetrics(trans.Started.total_bytes, trans.Started.resumed_bytes);
      setProgress({ transferred: trans.Started.resumed_bytes, total: trans.Started.total_bytes });
      setSpeedText(null);
      setEtaText(null);
    } else if ("Resumed" in trans) {
      setPhase("transferring");
      setStatus(`Resuming receive from ${formatBytes(trans.Resumed.resumed_bytes)}...`);
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
    } else if ("Completed" in trans) {
      terminalEventRef.current = true;
      setPhase("succeeded");
      setTransferFailed(false);
      setTechnicalDetails(null);
      const summary: TransferSummary = trans.Completed;
      const resumedBytes = summary.resumed_bytes ?? 0;
      const sessionBytes = Math.max(0, summary.total_bytes - resumedBytes);
      const path = outputDirRef.current
        ? `${outputDirRef.current}${outputDirRef.current.endsWith("\\") || outputDirRef.current.endsWith("/") ? "" : "\\"}${summary.file_name}`
        : null;
      addHistoryEntry({
        direction: "receive",
        fileName: summary.file_name,
        size: summary.total_bytes,
        resumedBytes,
        sessionBytes,
        peerName: summary.peer_name ?? summary.peer ?? "Unknown sender",
        durationMs: summary.elapsed_ms,
        mode: summary.mode,
        path: path ?? undefined,
        timestamp: new Date().toISOString(),
      });
      setStatus(
        summary.elapsed_ms != null
          ? (resumedBytes > 0
              ? `Received ${summary.file_name} in ${formatDuration(summary.elapsed_ms)} (Resumed from ${formatBytes(resumedBytes)})`
              : `Received ${summary.file_name} in ${formatDuration(summary.elapsed_ms)}`)
          : `Received ${summary.file_name} successfully!`
      );
      setSavedPath(path);
      setProgress(null);
      setSpeedText(null);
      setEtaText(null);
      metricsRef.current = null;
      if (successResetRef.current) clearTimeout(successResetRef.current);
      successResetRef.current = setTimeout(() => {
        resetReceiveUi();
      }, 5000);
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
    invoke<string | null>("get_device_name").then(setDeviceName).catch(() => setDeviceName(null));
    invoke<string | null>("get_local_ip").then(setLocalIp).catch(() => setLocalIp(null));
    invoke<string | null>("get_username").then(setUsername).catch(() => setUsername(null));
  }, []);

  useEffect(() => {
    if (mode !== "local") return;

    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    const prior = teardownRef.current;
    let markDone: () => void = () => {};
    teardownRef.current = new Promise<void>((resolve) => { markDone = resolve; });

    const setupReceiver = async () => {
      // Wait for any previous session to fully complete in the backend
      await prior;
      if (cancelled) return;
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
        setPhase("listening");
        try {
          const result = await invoke<TransferSummary>("receive_file_command", { request: req });
          void result;
          if (!cancelled) await new Promise(resolve => setTimeout(resolve, 1500));
        } catch (err) {
          if (!cancelled && !terminalEventRef.current) {
            terminalEventRef.current = true;
            setPhase("failed");
            setStatus(`Could not receive the file: ${err instanceof Error ? err.message : String(err)}`);
          }
          break;
        }
      }
    };

    setupReceiver().finally(() => markDone());

    return () => {
      cancelled = true;
      if (successResetRef.current) clearTimeout(successResetRef.current);
      // Abandon this session so a trailing event with same id can't hit the next mode.
      abandonSession(activeSessionRef, sessionFloorRef);
      if (unlisten) unlisten();
      // Signal the backend to stop. We can't await here (React cleanup is sync),
      // but the barrier above makes the next session wait for this one to settle.
      invoke("cancel_session_command", { sessionId: activeSessionRef.current }).catch(console.error);
    };
  }, [mode]);

  useEffect(() => {
    if (mode !== "internet") return;

    setStatus("Creating secure room...");
    setPhase("connecting");
    setRoomCode(null);
    let cancelled = false;
    const abortController = new AbortController();
    let unlisten: UnlistenFn | undefined;
    const prior = teardownRef.current;
    let markDone: () => void = () => {};
    teardownRef.current = new Promise<void>((resolve) => { markDone = resolve; });

    const setupRemoteReceiver = async () => {
      // Wait for any previous session to fully complete in the backend
      await prior;
      if (cancelled) return;
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

      if (cancelled) return;

      const receivePromise = invoke<TransferSummary>("receive_file_remote_command", { request: req });

      const regResult = await waitForRoomRegistration(
        RELAY_SERVER_URL,
        code,
        abortController.signal,
        10_000
      );

      if (cancelled) {
        invoke("cancel_session_command", { sessionId: code }).catch(console.error);
        return;
      }

      if (regResult.status !== "exists") {
        await invoke("cancel_session_command", { sessionId: code }).catch(console.error);
        if (!cancelled && !terminalEventRef.current) {
          terminalEventRef.current = true;
          setPhase("failed");
          const errorMsg = roomLookupMessage(regResult);
          setStatus(errorMsg || "Could not create the room. Try again.");
        }
        return;
      }

      setRoomCode(code);
      setPhase("listening");
      setStatus("Ready to receive files");

      try {
        const result = await receivePromise;
        void result;
      } catch (err) {
        if (!cancelled && !terminalEventRef.current) {
          terminalEventRef.current = true;
          setPhase("failed");
          setStatus(`Could not connect: ${err instanceof Error ? err.message : String(err)}`);
        }
      }
    };

    setupRemoteReceiver().finally(() => markDone());

    return () => {
      cancelled = true;
      abortController.abort();
      if (successResetRef.current) clearTimeout(successResetRef.current);
      // Abandon this session so a trailing event with same id can't hit the next mode.
      abandonSession(activeSessionRef, sessionFloorRef);
      if (unlisten) unlisten();
      invoke("cancel_session_command", { sessionId: activeSessionRef.current }).catch(console.error);
    };
  }, [mode]);

  useEffect(() => () => {
    if (successResetRef.current) clearTimeout(successResetRef.current);
    invoke("cancel_session_command", { sessionId: activeSessionRef.current }).catch(console.error);
  }, []);

  const isFailed = phase === "failed" || transferFailed;

  return (
    <div className="receive-container" data-phase={phase}>
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
            <h1 className="device-name" style={{ textAlign: "center" }}>
              {settings.deviceName || deviceName || <span style={{ fontStyle: "italic", opacity: 0.6 }}>Unknown device</span>}
            </h1>
            <div className="device-id" style={{ textAlign: "center", userSelect: "text" }}>
              {localIp
                ? <>{localIp}{port ? `:${port}` : ''}</>
                : <span style={{ fontStyle: "italic", opacity: 0.6 }}>Network address unavailable</span>}
              {username ? ` • ${username}` : ''}
            </div>
          </>
        )}

        {mode === "internet" && (
          <h1 className="device-name" style={{ textAlign: "center" }}>
            {settings.deviceName || deviceName || <span style={{ fontStyle: "italic", opacity: 0.6 }}>Unknown device</span>}
          </h1>
        )}

        <div style={{ marginTop: "20px", fontSize: "14px", color: "var(--text-secondary)", textAlign: "center" }}>
          {status}
        </div>

        {import.meta.env.DEV && isFailed && technicalDetails && (
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

        {copyError && (
          <div style={{ marginTop: "12px", fontSize: "12px", color: "#e5484d", textAlign: "center", maxWidth: "300px" }}>
            {copyError}
          </div>
        )}

        {progress && (
          <div style={{ marginTop: "16px", width: "80%", maxWidth: "300px" }}>
            <div style={{ width: "100%", backgroundColor: "var(--bg-sidebar)", height: "8px", borderRadius: "4px", overflow: "hidden" }}>
              <div style={{ width: `${progressPercent(progress.transferred, progress.total)}%`, backgroundColor: "var(--accent-primary)", height: "100%" }} />
            </div>
            <div style={{ fontSize: "12px", color: "var(--text-secondary)", marginTop: "8px", textAlign: "center" }}>
              <div style={{ display: "flex", justifyContent: "space-between" }}>
                <span>{Math.round(progressPercent(progress.transferred, progress.total))}%  •  {formatBytes(progress.transferred)} / {formatBytes(progress.total)}</span>
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
            <div style={{ display: "flex", gap: "8px", marginTop: "4px" }}>
              <button
                onClick={handleOpenFolder}
                style={{ display: "flex", alignItems: "center", gap: "6px", padding: "8px 16px", borderRadius: "8px", border: "none", backgroundColor: "var(--accent-primary)", color: "white", fontWeight: 600, cursor: "pointer", fontSize: "13px" }}
              >
                <FolderOpen size={16} />
                Open folder
              </button>
              <button
                onClick={resetReceiveUi}
                style={{ display: "flex", alignItems: "center", gap: "6px", padding: "8px 16px", borderRadius: "8px", border: "1px solid var(--border-color)", backgroundColor: "transparent", color: "var(--text-secondary)", fontWeight: 500, cursor: "pointer", fontSize: "13px" }}
              >
                Dismiss
              </button>
            </div>
          </div>
        )}
      </div>


    </div>
  );
};

export default ReceivePage;
