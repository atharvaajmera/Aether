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

  // Get the room code from the HTTPS endpoint from a `wss://.../ws`
  // relay signaling URL.
  static Uri? _roomUri(String relayServerUrl, String roomCode) {
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
      path: '/room/$roomCode',
    );
  }

  // Returns whether the relay currently has an open room for [roomCode].
  // Hits the relay's `GET /room/{code}` endpoint (204 = exists, 404 = gone).
  static Future<bool> roomExists(String relayServerUrl, String roomCode) async {
    final uri = _roomUri(relayServerUrl, roomCode);
    if (uri == null) return false;
    final resp = await http.get(uri).timeout(const Duration(seconds: 8));
    return resp.statusCode >= 200 && resp.statusCode < 300;
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
