import 'dart:convert';
import 'package:http/http.dart' as http;
import 'package:shared_preferences/shared_preferences.dart';

/// One STUN/TURN server entry, mirroring `plenum::signaling::IceServer`.
///
/// `urls` is kept as a single string in the UI/storage layer (like the
/// desktop app's settings), and wrapped into a one-element list when built
/// into the JSON payload the Rust FFI expects.
class IceServerSetting {
  String urls;
  String? username;
  String? credential;

  IceServerSetting({required this.urls, this.username, this.credential});

  factory IceServerSetting.fromJson(Map<String, dynamic> json) {
    return IceServerSetting(
      urls: json['urls'] as String? ?? '',
      username: json['username'] as String?,
      credential: json['credential'] as String?,
    );
  }

  Map<String, dynamic> toJson() => {
    'urls': urls,
    if (username != null && username!.isNotEmpty) 'username': username,
    if (credential != null && credential!.isNotEmpty) 'credential': credential,
  };

  /// Shape expected by the Rust side's `ice_servers_json` parameter:
  /// `{ urls: string[], username?: string, credential?: string }`.
  Map<String, dynamic> toIceServerJson() => {
    'urls': [urls],
    if (username != null && username!.isNotEmpty) 'username': username,
    if (credential != null && credential!.isNotEmpty) 'credential': credential,
  };
}

enum RoomLookupStatus {
  exists,
  notFound,
  relayUnavailable,
  invalidRelayUrl,
}

class RoomLookupResult {
  final RoomLookupStatus status;
  final int? statusCode;
  final Object? error;

  const RoomLookupResult({
    required this.status,
    this.statusCode,
    this.error,
  });

  bool get exists => status == RoomLookupStatus.exists;

  String get userMessage {
    switch (status) {
      case RoomLookupStatus.exists:
        return '';
      case RoomLookupStatus.notFound:
        return 'Room not found. Check the code or ask the receiver to create a new room.';
      case RoomLookupStatus.relayUnavailable:
        if (statusCode != null && statusCode! >= 500) {
          return 'The Plenum relay is temporarily unavailable. Try again shortly.';
        }
        return 'Couldn\'t reach the Plenum relay. Check your internet connection and try again.';
      case RoomLookupStatus.invalidRelayUrl:
        return 'Internet transfer is not configured correctly.';
    }
  }
}

const _relayServerUrlKey = 'internet.relay_server_url';
const _iceServersKey = 'internet.ice_servers';

/// Persists internet-transfer settings (relay server URL + ICE servers) via
/// shared_preferences, mirroring the desktop app's localStorage-backed
/// `SettingsContext`.
class InternetSettings {
  static const List<Map<String, String>> _defaultIceServers = [
    {'urls': 'stun:stun.l.google.com:19302'},
  ];

  static Future<String> loadRelayServerUrl() async {
    final prefs = await SharedPreferences.getInstance();
    return prefs.getString(_relayServerUrlKey) ?? '';
  }

  static Future<void> saveRelayServerUrl(String url) async {
    final prefs = await SharedPreferences.getInstance();
    await prefs.setString(_relayServerUrlKey, url);
  }

  static Future<List<IceServerSetting>> loadIceServers() async {
    final prefs = await SharedPreferences.getInstance();
    final raw = prefs.getString(_iceServersKey);
    if (raw == null) {
      return _defaultIceServers.map((s) => IceServerSetting.fromJson(s)).toList();
    }
    try {
      final decoded = jsonDecode(raw) as List<dynamic>;
      return decoded
          .map((e) => IceServerSetting.fromJson(e as Map<String, dynamic>))
          .toList();
    } catch (_) {
      return _defaultIceServers.map((s) => IceServerSetting.fromJson(s)).toList();
    }
  }

  static Future<void> saveIceServers(List<IceServerSetting> servers) async {
    final prefs = await SharedPreferences.getInstance();
    final encoded = jsonEncode(servers.map((s) => s.toJson()).toList());
    await prefs.setString(_iceServersKey, encoded);
  }

  /// Encodes the given servers into the JSON string the Rust FFI's
  /// `ice_servers_json` parameter expects.
  static String encodeIceServersForFfi(List<IceServerSetting> servers) {
    return jsonEncode(servers.map((s) => s.toIceServerJson()).toList());
  }

  /// Derives the `/turn-credentials` HTTPS endpoint from a `wss://.../ws`
  /// (or `ws://.../ws`) relay signaling URL.
  static Uri? _turnCredentialsUri(String relayServerUrl, String peerId) {
    final trimmed = relayServerUrl.trim();
    if (trimmed.isEmpty) return null;
    Uri parsed;
    try {
      parsed = Uri.parse(trimmed);
    } catch (_) {
      return null;
    }
    final httpsScheme = switch (parsed.scheme) {
      'wss' || 'https' => 'https',
      'ws' || 'http' => 'http',
      _ => 'https',
    };
    return parsed.replace(
      scheme: httpsScheme,
      path: '/turn-credentials',
      queryParameters: {'peer_id': peerId},
    );
  }

  /// Get the canonical HTTP/HTTPS room status URI from a relay URL (e.g. `wss://.../ws`)
  /// and room code.
  static Uri? roomStatusUri(String relayServerUrl, String roomCode) {
    final trimmed = relayServerUrl.trim();
    if (trimmed.isEmpty) return null;
    Uri parsed;
    try {
      parsed = Uri.parse(trimmed);
    } catch (_) {
      return null;
    }
    final httpsScheme = switch (parsed.scheme) {
      'wss' || 'https' => 'https',
      'ws' || 'http' => 'http',
      _ => 'https',
    };
    return Uri(
      scheme: httpsScheme,
      userInfo: parsed.userInfo.isNotEmpty ? parsed.userInfo : null,
      host: parsed.host,
      port: parsed.hasPort ? parsed.port : null,
      path: '/room/${Uri.encodeComponent(roomCode)}',
    );
  }

  /// Looks up whether an active room exists on the relay and returns a typed
  /// [RoomLookupResult] distinguishing 204 (exists), 404 (not found), 5xx / timeout
  /// (relay unavailable), or invalid URL configuration.
  static Future<RoomLookupResult> lookupRoom(
    String relayServerUrl,
    String roomCode, {
    Duration timeout = const Duration(seconds: 8),
  }) async {
    final uri = roomStatusUri(relayServerUrl, roomCode);
    if (uri == null) {
      return const RoomLookupResult(status: RoomLookupStatus.invalidRelayUrl);
    }
    try {
      final resp = await http.get(uri).timeout(timeout);
      if (resp.statusCode >= 200 && resp.statusCode < 300) {
        return RoomLookupResult(
          status: RoomLookupStatus.exists,
          statusCode: resp.statusCode,
        );
      }
      if (resp.statusCode == 404) {
        return const RoomLookupResult(
          status: RoomLookupStatus.notFound,
          statusCode: 404,
        );
      }
      return RoomLookupResult(
        status: RoomLookupStatus.relayUnavailable,
        statusCode: resp.statusCode,
      );
    } catch (e) {
      return RoomLookupResult(
        status: RoomLookupStatus.relayUnavailable,
        error: e,
      );
    }
  }

  // Returns whether the relay currently has an open room for [roomCode].
  // Hits the relay's `GET /room/{code}` endpoint (204 = exists, 404 = gone).
  static Future<bool> roomExists(String relayServerUrl, String roomCode) async {
    final result = await lookupRoom(relayServerUrl, roomCode);
    return result.exists;
  }

  /// Polls the relay status endpoint until the room exists or timeout expires.
  /// Uses an immediate initial check followed by bounded backoff (200ms -> 1000ms).
  static Future<RoomLookupResult> waitForRoomRegistration(
    String relayServerUrl,
    String roomCode, {
    Duration timeout = const Duration(seconds: 10),
    bool Function()? isCancelled,
  }) async {
    final stopwatch = Stopwatch()..start();
    var delayMs = 200;
    RoomLookupResult lastResult = const RoomLookupResult(
      status: RoomLookupStatus.notFound,
      statusCode: 404,
    );

    while (stopwatch.elapsed < timeout) {
      if (isCancelled?.call() == true) {
        return lastResult;
      }

      final remaining = timeout - stopwatch.elapsed;
      lastResult = await lookupRoom(
        relayServerUrl,
        roomCode,
        timeout: remaining > const Duration(seconds: 2)
            ? const Duration(seconds: 2)
            : (remaining > Duration.zero ? remaining : const Duration(milliseconds: 100)),
      );

      if (lastResult.exists) {
        return lastResult;
      }

      if (isCancelled?.call() == true) {
        return lastResult;
      }

      final remainingAfterLookup = timeout - stopwatch.elapsed;
      if (remainingAfterLookup <= Duration.zero) break;

      final sleepMs = delayMs.clamp(0, remainingAfterLookup.inMilliseconds);
      await Future.delayed(Duration(milliseconds: sleepMs));
      delayMs = (delayMs + 200).clamp(200, 1000);
    }

    return lastResult;
  }

  /// Looks up whether an active room exists on the relay, retrying `404` across
  /// a short grace period (default: 4 seconds) to tolerate registration propagation races.
  static Future<RoomLookupResult> lookupRoomWithGracePeriod(
    String relayServerUrl,
    String roomCode, {
    Duration timeout = const Duration(seconds: 4),
    Duration pollInterval = const Duration(milliseconds: 500),
    bool Function()? isCancelled,
  }) async {
    final stopwatch = Stopwatch()..start();
    RoomLookupResult lastResult = const RoomLookupResult(
      status: RoomLookupStatus.notFound,
      statusCode: 404,
    );

    while (stopwatch.elapsed < timeout) {
      if (isCancelled?.call() == true) {
        return lastResult;
      }

      final remaining = timeout - stopwatch.elapsed;
      lastResult = await lookupRoom(
        relayServerUrl,
        roomCode,
        timeout: remaining > const Duration(seconds: 2)
            ? const Duration(seconds: 2)
            : (remaining > Duration.zero ? remaining : const Duration(milliseconds: 100)),
      );

      if (lastResult.exists) {
        return lastResult;
      }

      if (lastResult.status == RoomLookupStatus.invalidRelayUrl) {
        return lastResult;
      }

      if (isCancelled?.call() == true) {
        return lastResult;
      }

      final remainingAfterLookup = timeout - stopwatch.elapsed;
      if (remainingAfterLookup <= Duration.zero) break;

      final sleepMs = pollInterval.inMilliseconds.clamp(0, remainingAfterLookup.inMilliseconds);
      await Future.delayed(Duration(milliseconds: sleepMs));
    }

    return lastResult;
  }

  /// Builds the full `ice_servers_json` FFI payload for a transfer: the
  /// user-configured STUN/TURN servers plus freshly minted, short-lived TURN
  /// credentials fetched from the relay's `/turn-credentials` endpoint.
  ///
  /// The fetch is best-effort: if the relay has no TURN configured, is
  /// unreachable, or returns an error, we silently fall back to just the
  /// configured servers (STUN-only still works for most NATs).
  static Future<String> buildIceServersJsonWithTurn(
    String relayServerUrl,
    String peerId,
    List<IceServerSetting> configured,
  ) async {
    final maps = configured.map((s) => s.toIceServerJson()).toList();

    final uri = _turnCredentialsUri(relayServerUrl, peerId);
    if (uri != null) {
      try {
        final resp = await http
            .get(uri)
            .timeout(const Duration(seconds: 8));
        if (resp.statusCode == 200) {
          final body = jsonDecode(resp.body) as Map<String, dynamic>;
          final urls = (body['urls'] as List<dynamic>?)?.cast<String>() ?? [];
          if (urls.isNotEmpty) {
            maps.add({
              'urls': urls,
              if (body['username'] != null) 'username': body['username'],
              if (body['credential'] != null) 'credential': body['credential'],
            });
          }
        }
      } catch (_) {
        // Best-effort: fall back to configured servers only.
      }
    }

    return jsonEncode(maps);
  }
}
