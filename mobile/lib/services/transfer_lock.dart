import 'dart:io';
import 'package:flutter/services.dart';

/// Holds a Wi-Fi high-performance lock + partial CPU wake lock on Android for
/// the duration of a transfer.
class TransferLock {
  static const MethodChannel _channel = MethodChannel('plenum/media');

  /// Acquire both locks. errors are surfaced
  static Future<void> acquire() async {
    if (!Platform.isAndroid) return;
    final acquired = await _channel.invokeMethod<bool>('acquireTransferLock');
    if (acquired != true) {
      throw StateError('Android transfer lock was not acquired');
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
