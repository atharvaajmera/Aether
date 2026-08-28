// transfer history page in desktop settings
import React, { useState } from "react";
import { Upload, Download, FolderOpen, Trash2 } from "lucide-react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { getHistory, clearHistory, HistoryEntry } from "../services/history";
import { formatBytes, formatDuration, formatTransferMode } from "../utils/format";

const HistoryPage: React.FC = () => {
  // Read once on mount — no transfer can complete while the user sits here.
  const [entries, setEntries] = useState<HistoryEntry[]>(() => getHistory());
  // Shown when revealing a file fails (moved, deleted, drive offline, denied).
  const [revealError, setRevealError] = useState<string | null>(null);

  const handleClear = () => {
    clearHistory();
    setEntries([]);
    setRevealError(null);
  };

  const handleReveal = async (path: string, fileName: string) => {
    try {
      await revealItemInDir(path);
      setRevealError(null);
    } catch (err) {
      console.error("Reveal failed:", err);
      setRevealError(`Couldn't open "${fileName}" — it may have been moved, deleted, or saved to a drive that isn't connected.`);
    }
  };

  return (
    <div className="settings-container">
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: "24px" }}>
        <h1 className="settings-title" style={{ marginBottom: 0 }}>Transfer History</h1>
        {entries.length > 0 && (
          <button
            onClick={handleClear}
            style={{ display: "flex", alignItems: "center", gap: "6px", padding: "8px 14px", borderRadius: "8px", border: "1px solid var(--border-color)", background: "none", color: "var(--text-secondary)", cursor: "pointer", fontSize: "13px" }}
          >
            <Trash2 size={15} />
            Clear history
          </button>
        )}
      </div>

      {revealError && (
        <div
          role="alert"
          onClick={() => setRevealError(null)}
          style={{ marginBottom: "16px", padding: "12px 16px", borderRadius: "8px", backgroundColor: "color-mix(in srgb, #e5484d 12%, transparent)", border: "1px solid color-mix(in srgb, #e5484d 40%, transparent)", color: "var(--text-primary)", fontSize: "13px", cursor: "pointer" }}
          title="Dismiss"
        >
          {revealError}
        </div>
      )}

      {entries.length === 0 ? (
        <div style={{ padding: "48px", textAlign: "center", color: "var(--text-secondary)" }}>
          No past transfers
        </div>
      ) : (
        <div className="settings-card">
          {entries.map((e, i) => {
            const isSend = e.direction === "send";
            const durationStr = e.durationMs != null ? formatDuration(e.durationMs) : "—";
            const modeLabel = formatTransferMode(e.mode) ?? "—";
            const timeStr = e.timestamp?.slice(0, 16) ?? "";
            const canReveal = !isSend && !!e.path;
            return (
              <div
                key={i}
                className="settings-row"
                style={{ alignItems: "flex-start", gap: "12px", cursor: canReveal ? "pointer" : "default" }}
                onClick={canReveal ? () => handleReveal(e.path!, e.fileName) : undefined}
              >
                <div style={{ display: "flex", alignItems: "center", justifyContent: "center", width: "36px", height: "36px", borderRadius: "50%", backgroundColor: "var(--bg-sidebar)", flexShrink: 0 }}>
                  {isSend
                    ? <Upload size={18} color="var(--accent-primary)" />
                    : <Download size={18} color="#4a90e2" />}
                </div>
                <div style={{ display: "flex", flexDirection: "column", gap: "4px", flex: 1, minWidth: 0 }}>
                  <span style={{ color: "var(--text-primary)", fontWeight: 500, wordBreak: "break-all" }}>{e.fileName}</span>
                  <span style={{ fontSize: "12px", color: "var(--text-secondary)" }}>
                    {isSend ? "To" : "From"} {e.peerName} • {formatBytes(e.size)} • {durationStr}
                    {e.resumedBytes && e.resumedBytes > 0 ? ` (Resumed from ${formatBytes(e.resumedBytes)})` : ""}
                  </span>
                  <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
                    <span style={{ fontSize: "10px", fontWeight: 600, color: "var(--accent-primary)", padding: "2px 8px", borderRadius: "10px", backgroundColor: "color-mix(in srgb, var(--accent-primary) 12%, transparent)", border: "1px solid color-mix(in srgb, var(--accent-primary) 35%, transparent)" }}>
                      {modeLabel}
                    </span>
                    <span style={{ fontSize: "11px", color: "var(--text-secondary)" }}>{timeStr}</span>
                  </div>
                </div>
                {canReveal && (
                  <FolderOpen size={16} color="var(--text-secondary)" style={{ flexShrink: 0, marginTop: "8px" }} />
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
};

export default HistoryPage;
