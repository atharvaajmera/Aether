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
  timestamp: string; // ISO-8601
}

const STORAGE_KEY = "plenum-history";
const MAX_ENTRIES = 100;

export function getHistory(): HistoryEntry[] {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (!stored) return [];
    const parsed = JSON.parse(stored);
    return Array.isArray(parsed) ? parsed : [];
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
