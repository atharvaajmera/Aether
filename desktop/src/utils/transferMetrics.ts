export interface TransferMetricsState {
  resumedBytes: number;
  transferredBytes: number;
  totalBytes: number;
  startedAtMs: number;
  lastSampleAtMs: number;
  lastSampleBytes: number;
  smoothedBytesPerSecond: number | null;
}

export interface TransferMetricsUpdate {
  speedBps: number | null;
  etaSeconds: number | null;
  progressFraction: number;
  sessionTransferredBytes: number;
  remainingBytes: number;
}

export function createTransferMetrics(
  totalBytes: number,
  resumedBytes: number = 0,
  startedAtMs: number = Date.now()
): TransferMetricsState {
  return {
    resumedBytes,
    transferredBytes: resumedBytes,
    totalBytes,
    startedAtMs,
    lastSampleAtMs: startedAtMs,
    lastSampleBytes: resumedBytes,
    smoothedBytesPerSecond: null,
  };
}

export function updateTransferMetrics(
  state: TransferMetricsState,
  currentTransferredBytes: number,
  nowMs: number = Date.now()
): TransferMetricsUpdate {
  state.transferredBytes = Math.min(currentTransferredBytes, state.totalBytes);
  const timeDeltaSec = (nowMs - state.lastSampleAtMs) / 1000;

  // Ignore samples shorter than ~250ms to avoid noisy values
  if (timeDeltaSec >= 0.25) {
    const bytesDelta = Math.max(0, currentTransferredBytes - state.lastSampleBytes);
    const instantSpeed = bytesDelta / timeDeltaSec;

    if (state.smoothedBytesPerSecond === null) {
      state.smoothedBytesPerSecond = instantSpeed;
    } else {
      state.smoothedBytesPerSecond =
        state.smoothedBytesPerSecond * 0.7 + instantSpeed * 0.3;
    }

    state.lastSampleAtMs = nowMs;
    state.lastSampleBytes = currentTransferredBytes;
  }

  const remainingBytes = Math.max(0, state.totalBytes - currentTransferredBytes);
  const sessionTransferredBytes = Math.max(
    0,
    currentTransferredBytes - state.resumedBytes
  );

  let etaSeconds: number | null = null;
  if (state.smoothedBytesPerSecond && state.smoothedBytesPerSecond > 0) {
    etaSeconds = Math.round(remainingBytes / state.smoothedBytesPerSecond);
  }

  const progressFraction =
    state.totalBytes > 0
      ? Math.min(1.0, Math.max(0.0, currentTransferredBytes / state.totalBytes))
      : 0.0;

  return {
    speedBps: state.smoothedBytesPerSecond,
    etaSeconds,
    progressFraction,
    sessionTransferredBytes,
    remainingBytes,
  };
}
