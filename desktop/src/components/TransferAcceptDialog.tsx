import React, { useEffect, useState } from "react";
import { FileDown } from "lucide-react";
import { formatBytes } from "../utils/format";

export interface IncomingTransfer {
  fileName: string;
  totalBytes: number;
  peer?: string;
  senderName?: string;
}

interface Props {
  incoming: IncomingTransfer;
  onRespond: (accept: boolean) => void;
}

/** Matches the engine's APPROVAL_TIMEOUT: it auto-declines after 120 s. */
const APPROVAL_TIMEOUT_SECS = 120;

/**
 * Modal accept gate shown when the engine emits `IncomingRequest` and
 * quick-save (auto-accept) is off. The Rust loop blocks until we answer or
 * its own approval timeout declines for us.
 */
const TransferAcceptDialog: React.FC<Props> = ({ incoming, onRespond }) => {
  const [secondsLeft, setSecondsLeft] = useState(APPROVAL_TIMEOUT_SECS);

  useEffect(() => {
    setSecondsLeft(APPROVAL_TIMEOUT_SECS);
    const interval = setInterval(() => {
      setSecondsLeft((prev) => {
        if (prev <= 1) {
          clearInterval(interval);
          // Engine declines on its own; just dismiss the dialog.
          onRespond(false);
          return 0;
        }
        return prev - 1;
      });
    }, 1000);
    return () => clearInterval(interval);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [incoming]);

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        backgroundColor: "rgba(0, 0, 0, 0.6)",
        zIndex: 200,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      <div
        style={{
          backgroundColor: "var(--bg-card)",
          border: "1px solid var(--border-color)",
          borderRadius: "12px",
          padding: "24px",
          minWidth: "340px",
          maxWidth: "420px",
          boxShadow: "0 8px 24px rgba(0,0,0,0.4)",
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: "12px", marginBottom: "16px" }}>
          <FileDown size={28} color="var(--accent-primary)" />
          <div style={{ fontSize: "16px", fontWeight: 600, color: "var(--text-primary)" }}>Incoming file</div>
        </div>

        <div style={{ fontSize: "14px", fontWeight: 600, color: "var(--text-primary)", wordBreak: "break-all" }}>
          {incoming.fileName}
        </div>
        <div style={{ fontSize: "13px", color: "var(--text-secondary)", marginTop: "4px" }}>
          {formatBytes(incoming.totalBytes)}
        </div>

        {/* Device name is the primary identity; raw address/session id only a
            fallback when the sender is an older build without one. */}
        {incoming.senderName ? (
          <div style={{ marginTop: "12px" }}>
            <div style={{ fontSize: "14px", fontWeight: 600, color: "var(--text-primary)" }}>
              From: {incoming.senderName}
            </div>
            {incoming.peer && (
              <div style={{ fontSize: "11px", color: "var(--text-secondary)", marginTop: "2px" }}>{incoming.peer}</div>
            )}
          </div>
        ) : incoming.peer ? (
          <div style={{ marginTop: "12px", fontSize: "13px", color: "var(--text-secondary)" }}>
            From: {incoming.peer}
          </div>
        ) : null}

        <div style={{ marginTop: "12px", fontSize: "12px", color: "var(--text-secondary)" }}>
          Auto-declines in {secondsLeft}s
        </div>

        <div style={{ display: "flex", gap: "12px", marginTop: "20px", justifyContent: "flex-end" }}>
          <button
            onClick={() => onRespond(false)}
            style={{
              padding: "10px 20px",
              borderRadius: "8px",
              border: "1px solid var(--border-color)",
              backgroundColor: "transparent",
              color: "var(--text-secondary)",
              fontWeight: 600,
              cursor: "pointer",
            }}
          >
            Decline
          </button>
          <button
            onClick={() => onRespond(true)}
            style={{
              padding: "10px 20px",
              borderRadius: "8px",
              border: "none",
              backgroundColor: "var(--accent-primary)",
              color: "white",
              fontWeight: 600,
              cursor: "pointer",
            }}
          >
            Accept
          </button>
        </div>
      </div>
    </div>
  );
};

export default TransferAcceptDialog;
