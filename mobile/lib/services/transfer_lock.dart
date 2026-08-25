import 'dart:io';
import 'package:flutter/services.dart';

/// Holds a Wi-Fi high-performance lock + partial CPU wake lock on Android for
/// the duration of a transfer.
class TransferLock {
  static const MethodChannel _channel = MethodChannel('plenum/media');

  /// Token of the transfer that currently holds the lock, or null if free.
  static Object? _owner;

  /// Acquire both locks. Returns an opaque ownership token to pass back to
  /// [release]. Errors are surfaced.
  static Future<Object> acquire() async {
    final token = Object();
    if (Platform.isAndroid) {
      final acquired = await _channel.invokeMethod<bool>('acquireTransferLock');
      if (acquired != true) {
        throw StateError('Android transfer lock was not acquired');
      }
    }
    // Set ownership after a successful native acquire so a failed acquire
    // doesn't leave a stale owner.
    _owner = token;
    return token;
  }

  // Release the locks, if [token] still owns them.
  static Future<void> release(Object? token) async {
    if (token == null || !identical(_owner, token)) return;
    _owner = null;
    if (!Platform.isAndroid) return;
    try {
      await _channel.invokeMethod('releaseTransferLock');
    } catch (_) {
      // best
    }
  }
}
