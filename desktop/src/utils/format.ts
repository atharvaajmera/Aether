export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

export function progressPercent(transferred: number, total: number): number {
  if (!Number.isFinite(transferred) || !Number.isFinite(total)) return 0;
  if (total <= 0) return total === 0 && transferred >= 0 ? 100 : 0;
  const pct = (transferred / total) * 100;
  return Math.min(100, Math.max(0, pct));
}

/** Humanizes a transfer duration: "12.4 s" under a minute, "1 m 05 s" above. */
export function formatDuration(ms: number): string {
  const totalSeconds = ms / 1000;
  if (totalSeconds < 60) return `${totalSeconds.toFixed(1)} s`;
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = Math.round(totalSeconds % 60);
  return `${minutes} m ${String(seconds).padStart(2, "0")} s`;
}

/**
 * Maps a `TransferSummary.mode` value to a display label. Unknown/missing
 * values return null so old history entries can render a placeholder.
 */
export function formatTransferMode(mode: unknown): string | null {
  switch (mode) {
    case "Lan":
      return "LAN";
    case "Direct":
      return "Direct";
    case "Relay":
      return "Relay";
    default:
      return null;
  }
}
