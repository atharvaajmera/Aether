import { TransferMode } from "../types/rust";

export interface HistoryEntry {
  direction: "send" | "receive";
  fileName: string;
  size: number;
  /** Peer device name when known, else raw address/session id. */
  peerName: string;
  durationMs?: number;
  mode?: TransferMode;
  /** Local path of the received file (receive entries only). */
  path?: string;
  resumedBytes?: number;
  sessionBytes?: number;
  timestamp: string; // ISO-8601
}

const STORAGE_KEY = "plenum-history";
const MAX_ENTRIES = 100;

function isValidEntry(value: unknown): value is HistoryEntry {
  if (typeof value !== "object" || value === null) return false;
  const e = value as Record<string, unknown>;
  if (e.direction !== "send" && e.direction !== "receive") return false;
  if (typeof e.fileName !== "string") return false;
  if (typeof e.size !== "number" || !Number.isFinite(e.size)) return false;
  if (typeof e.peerName !== "string") return false;
  if (typeof e.timestamp !== "string") return false;
  if (e.durationMs !== undefined && typeof e.durationMs !== "number") return false;
  if (e.mode !== undefined && typeof e.mode !== "string") return false;
  if (e.path !== undefined && typeof e.path !== "string") return false;
  if (e.resumedBytes !== undefined && typeof e.resumedBytes !== "number") return false;
  if (e.sessionBytes !== undefined && typeof e.sessionBytes !== "number") return false;
  return true;
}

export function getHistory(): HistoryEntry[] {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (!stored) return [];
    const parsed = JSON.parse(stored);
    return Array.isArray(parsed) ? parsed.filter(isValidEntry) : [];
  } catch {
    return [];
  }
}

/** Prepends an entry (newest-first) and trims to the last 100. */
export function addHistoryEntry(entry: HistoryEntry): void {
  const list = getHistory();
  list.unshift(entry);
  if (list.length > MAX_ENTRIES) list.length = MAX_ENTRIES;
  localStorage.setItem(STORAGE_KEY, JSON.stringify(list));
}

export function clearHistory(): void {
  localStorage.removeItem(STORAGE_KEY);
}
