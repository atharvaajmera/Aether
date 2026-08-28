import 'dart:math';

class TransferMetricsState {
  final int totalBytes;
  final int resumedBytes;
  int transferredBytes;
  final DateTime startedAt;
  DateTime lastSampleAt;
  int lastSampleBytes;
  double? smoothedBytesPerSecond;

  TransferMetricsState({
    required this.totalBytes,
    this.resumedBytes = 0,
    required this.startedAt,
    required this.lastSampleAt,
    required this.lastSampleBytes,
    this.smoothedBytesPerSecond,
  }) : transferredBytes = resumedBytes;

  factory TransferMetricsState.start({
    required int totalBytes,
    int resumedBytes = 0,
    DateTime? startedAt,
  }) {
    final now = startedAt ?? DateTime.now();
    return TransferMetricsState(
      totalBytes: totalBytes,
      resumedBytes: resumedBytes,
      startedAt: now,
      lastSampleAt: now,
      lastSampleBytes: resumedBytes,
    );
  }

  TransferMetricsUpdate update(int currentTransferredBytes, [DateTime? now]) {
    final nowTime = now ?? DateTime.now();
    transferredBytes = currentTransferredBytes.clamp(0, totalBytes);
    final elapsedMs = nowTime.difference(lastSampleAt).inMilliseconds;

    // Ignore samples shorter than ~250ms to avoid noisy values
    if (elapsedMs >= 250) {
      final bytesDelta = max(0, currentTransferredBytes - lastSampleBytes);
      final instantSpeed = (bytesDelta * 1000) / elapsedMs;

      if (smoothedBytesPerSecond == null) {
        smoothedBytesPerSecond = instantSpeed;
      } else {
        smoothedBytesPerSecond =
            (smoothedBytesPerSecond! * 0.7) + (instantSpeed * 0.3);
      }

      lastSampleAt = nowTime;
      lastSampleBytes = currentTransferredBytes;
    }

    final remainingBytes = max(0, totalBytes - currentTransferredBytes);
    final sessionTransferredBytes = max(0, currentTransferredBytes - resumedBytes);

    int? etaSeconds;
    if (smoothedBytesPerSecond != null && smoothedBytesPerSecond! > 0) {
      etaSeconds = (remainingBytes / smoothedBytesPerSecond!).round();
    }

    final progressFraction = totalBytes > 0
        ? (currentTransferredBytes / totalBytes).clamp(0.0, 1.0)
        : 0.0;

    return TransferMetricsUpdate(
      speedBps: smoothedBytesPerSecond,
      etaSeconds: etaSeconds,
      progressFraction: progressFraction,
      sessionTransferredBytes: sessionTransferredBytes,
      remainingBytes: remainingBytes,
    );
  }
}

class TransferMetricsUpdate {
  final double? speedBps;
  final int? etaSeconds;
  final double progressFraction;
  final int sessionTransferredBytes;
  final int remainingBytes;

  const TransferMetricsUpdate({
    required this.speedBps,
    required this.etaSeconds,
    required this.progressFraction,
    required this.sessionTransferredBytes,
    required this.remainingBytes,
  });
}
