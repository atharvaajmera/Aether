export type RoomLookupStatus =
  | "exists"
  | "not-found"
  | "relay-unavailable"
  | "invalid-relay-url";

export type RoomLookupResult =
  | { status: "exists"; statusCode?: number }
  | { status: "not-found"; statusCode: 404 }
  | { status: "relay-unavailable"; statusCode?: number; error?: unknown }
  | { status: "invalid-relay-url"; error?: unknown };

export function relayHttpUrl(
  relayWebSocketUrl: string,
  path: string,
): string {
  if (!relayWebSocketUrl || !relayWebSocketUrl.trim()) {
    throw new Error("Empty relay URL");
  }

  const url = new URL(relayWebSocketUrl);

  if (url.protocol === "wss:") {
    url.protocol = "https:";
  } else if (url.protocol === "ws:") {
    url.protocol = "http:";
  } else if (url.protocol !== "https:" && url.protocol !== "http:") {
    throw new Error(`Unsupported relay protocol: ${url.protocol}`);
  }

  url.pathname = path.startsWith("/") ? path : `/${path}`;
  url.search = "";
  url.hash = "";

  return url.toString();
}

export function roomStatusUrl(
  relayWebSocketUrl: string,
  roomCode: string,
): string {
  return relayHttpUrl(
    relayWebSocketUrl,
    `/room/${encodeURIComponent(roomCode)}`,
  );
}

export function roomLookupMessage(result: RoomLookupResult): string {
  switch (result.status) {
    case "exists":
      return "";
    case "not-found":
      return "Room not found. Check the code or ask the receiver to create a new room.";
    case "relay-unavailable":
      if (result.statusCode && result.statusCode >= 500) {
        return "The Plenum relay is temporarily unavailable. Try again shortly.";
      }
      return "Couldn't reach the Plenum relay. Check your internet connection and try again.";
    case "invalid-relay-url":
      return "Internet transfer is not configured correctly.";
  }
}

export async function lookupRoom(
  relayWebSocketUrl: string,
  roomCode: string,
  timeoutMs: number = 8000
): Promise<RoomLookupResult> {
  let url: string;
  try {
    url = roomStatusUrl(relayWebSocketUrl, roomCode);
  } catch (err) {
    return { status: "invalid-relay-url", error: err };
  }

  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);

  try {
    const response = await fetch(url, { signal: controller.signal });
    clearTimeout(timer);

    if (response.status >= 200 && response.status < 300) {
      return { status: "exists", statusCode: response.status };
    }
    if (response.status === 404) {
      return { status: "not-found", statusCode: 404 };
    }
    return { status: "relay-unavailable", statusCode: response.status };
  } catch (err) {
    clearTimeout(timer);
    return { status: "relay-unavailable", error: err };
  }
}

export async function waitForRoomRegistration(
  relayUrl: string,
  roomCode: string,
  signal?: AbortSignal,
  timeoutMs: number = 10_000,
): Promise<RoomLookupResult> {
  const start = Date.now();
  let delayMs = 200;
  let lastResult: RoomLookupResult = { status: "not-found", statusCode: 404 };

  while (Date.now() - start < timeoutMs) {
    if (signal?.aborted) {
      return lastResult;
    }

    const remainingTimeout = timeoutMs - (Date.now() - start);
    lastResult = await lookupRoom(
      relayUrl,
      roomCode,
      Math.min(2000, remainingTimeout > 0 ? remainingTimeout : 1000)
    );

    if (lastResult.status === "exists") {
      return lastResult;
    }

    if (signal?.aborted) {
      return lastResult;
    }

    const remaining = timeoutMs - (Date.now() - start);
    if (remaining <= 0) break;

    const sleepMs = Math.min(delayMs, remaining);
    await new Promise<void>((resolve) => {
      const timer = setTimeout(resolve, sleepMs);
      if (signal) {
        const onAbort = () => {
          clearTimeout(timer);
          signal.removeEventListener("abort", onAbort);
          resolve();
        };
        signal.addEventListener("abort", onAbort, { once: true });
      }
    });

    delayMs = Math.min(1000, delayMs + 200);
  }

  return lastResult;
}

export async function lookupRoomWithGracePeriod(
  relayUrl: string,
  roomCode: string,
  signal?: AbortSignal,
  timeoutMs: number = 4000,
  pollIntervalMs: number = 500
): Promise<RoomLookupResult> {
  const start = Date.now();
  let lastResult: RoomLookupResult = { status: "not-found", statusCode: 404 };

  while (Date.now() - start < timeoutMs) {
    if (signal?.aborted) return lastResult;

    const remainingTimeout = timeoutMs - (Date.now() - start);
    lastResult = await lookupRoom(
      relayUrl,
      roomCode,
      Math.min(2000, remainingTimeout > 0 ? remainingTimeout : 1000)
    );

    if (lastResult.status === "exists") {
      return lastResult;
    }

    if (lastResult.status === "invalid-relay-url") {
      return lastResult;
    }

    if (signal?.aborted) return lastResult;

    const remaining = timeoutMs - (Date.now() - start);
    if (remaining <= 0) break;

    const sleepMs = Math.min(pollIntervalMs, remaining);
    await new Promise<void>((resolve) => {
      const timer = setTimeout(resolve, sleepMs);
      if (signal) {
        const onAbort = () => {
          clearTimeout(timer);
          signal.removeEventListener("abort", onAbort);
          resolve();
        };
        signal.addEventListener("abort", onAbort, { once: true });
      }
    });
  }

  return lastResult;
}
