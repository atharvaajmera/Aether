import 'dart:io';
import 'package:flutter/services.dart';

/// Holds a Wi-Fi high-performance lock + partial CPU wake lock on Android for
/// the duration of a transfer.
class TransferLock {
  static const MethodChannel _channel = MethodChannel('plenum/media');

  /// Acquire the locks. Safe to call repeatedly; the native side is idempotent.
  static Future<void> acquire() async {
    if (!Platform.isAndroid) return;
    try {
      await _channel.invokeMethod('acquireTransferLock');
    } catch (_) {
      // Best-effort: a lock failure must never block a transfer.
    }
  }

  /// Release the locks. Safe to call even if none were acquired.
  static Future<void> release() async {
    if (!Platform.isAndroid) return;
    try {
      await _channel.invokeMethod('releaseTransferLock');
    } catch (_) {
      // best case
    }
  }
}
