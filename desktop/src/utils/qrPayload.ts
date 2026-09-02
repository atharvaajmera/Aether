// Helper utilities for encoding and decoding QR payloads.
export function encodeRoomQr(code: string): string {
  const trimmed = code.trim().toUpperCase();
  return `plenum://room/${encodeURIComponent(trimmed)}`;
}

export function encodePinQr(pin: string): string {
  const trimmed = pin.trim();
  return `plenum://pin/${encodeURIComponent(trimmed)}`;
}

export interface ParsedQrPayload {
  type: "room" | "pin" | "raw";
  code: string;
}

export function parseQrPayload(raw: string): ParsedQrPayload | null {
  const trimmed = raw.trim();
  if (!trimmed) return null;

  if (trimmed.startsWith("plenum://")) {
    const withoutPrefix = trimmed.slice("plenum://".length);
    const slashIdx = withoutPrefix.indexOf("/");
    if (slashIdx === -1) {
      return null;
    }
    const type = withoutPrefix.slice(0, slashIdx).toLowerCase();
    const code = decodeURIComponent(withoutPrefix.slice(slashIdx + 1)).trim();
    if (!code) return null;

    if (type === "room") {
      return { type: "room", code: code.toUpperCase() };
    }
    if (type === "pin") {
      return { type: "pin", code };
    }
    return null;
  }

  // Fallback for plain text codes
  return { type: "raw", code: trimmed };
}
