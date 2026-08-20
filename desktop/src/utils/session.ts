import type { MutableRefObject } from "react";

export function isStaleSession(
  sessionId: number,
  activeRef: MutableRefObject<number>,
  floorRef: MutableRefObject<number>,
): boolean {
  if (sessionId === 0) return false;
  if (sessionId < activeRef.current) return true;
  if (sessionId <= floorRef.current) return true;
  activeRef.current = sessionId;
  return false;
}

// Call when a page intentionally leaves a session (effect cleanup, mode switch)
export function abandonSession(
  activeRef: MutableRefObject<number>,
  floorRef: MutableRefObject<number>,
): void {
  floorRef.current = Math.max(floorRef.current, activeRef.current);
}
