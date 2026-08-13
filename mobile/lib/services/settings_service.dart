import 'dart:convert';
import 'dart:io';
import 'package:flutter/material.dart';
import 'package:shared_preferences/shared_preferences.dart';
import 'package:device_info_plus/device_info_plus.dart';

import '../config.dart';

class SettingsService extends ChangeNotifier {
  final SharedPreferences _prefs;
  
  SettingsService(this._prefs);

  static Future<SettingsService> init() async {
    final prefs = await SharedPreferences.getInstance();
    final service = SettingsService(prefs);
    await service._ensureDefaults();
    return service;
  }

  Future<void> _ensureDefaults() async {
    // Default Mode was removed; screens always open on Local. Drop the stale key.
    await _prefs.remove('defaultTransferMode');
    // Migrate away from the stale relay default an earlier build persisted;
    // clearing it lets the PlenumConfig-derived getter default take over.
    if (_prefs.getString('relayServerUrl') == 'wss://plenum-relay.fly.dev/ws') {
      await _prefs.remove('relayServerUrl');
    }
    if (_prefs.getString('iceServers') ==
        'stun:stun.l.google.com:19302\nstun:stun1.l.google.com:19302') {
      await _prefs.remove('iceServers');
    }
    if (_prefs.getString('deviceName') == null) {
      String name = 'Unknown Device';
      try {
        final deviceInfo = DeviceInfoPlugin();
        if (Platform.isAndroid) {
          final info = await deviceInfo.androidInfo;
          name = info.model;
        } else if (Platform.isIOS) {
          final info = await deviceInfo.iosInfo;
          name = info.utsname.machine;
        }
      } catch (_) {}
      await _prefs.setString('deviceName', name);
    }
  }

  bool get requirePin => _prefs.getBool('requirePin') ?? false;
  Future<void> setRequirePin(bool value) async {
    await _prefs.setBool('requirePin', value);
    notifyListeners();
  }

  String get deviceName => _prefs.getString('deviceName') ?? 'Unknown Device';
  Future<void> setDeviceName(String value) async {
    await _prefs.setString('deviceName', value);
    notifyListeners();
  }

  bool get autoAccept => _prefs.getBool('autoAccept') ?? false;
  Future<void> setAutoAccept(bool value) async {
    await _prefs.setBool('autoAccept', value);
    notifyListeners();
  }

  ThemeMode get themeMode {
    final val = _prefs.getInt('themeMode') ?? 0;
    if (val == 1) return ThemeMode.light;
    if (val == 2) return ThemeMode.dark;
    return ThemeMode.system;
  }
  Future<void> setThemeMode(ThemeMode mode) async {
    int val = 0;
    if (mode == ThemeMode.light) val = 1;
    if (mode == ThemeMode.dark) val = 2;
    await _prefs.setInt('themeMode', val);
    notifyListeners();
  }

  String get relayServerUrl => _prefs.getString('relayServerUrl') ?? PlenumConfig.relayServerUrl;
  Future<void> setRelayServerUrl(String value) async {
    await _prefs.setString('relayServerUrl', value);
    notifyListeners();
  }

  String get iceServers =>
      _prefs.getString('iceServers') ??
      PlenumConfig.defaultIceServers().map((s) => s.urls).join('\n');
  Future<void> setIceServers(String value) async {
    await _prefs.setString('iceServers', value);
    notifyListeners();
  }

  List<Map<String, dynamic>> get transferHistory {
    final list = _prefs.getStringList('transferHistory') ?? [];
    return list.map((s) => jsonDecode(s) as Map<String, dynamic>).toList();
  }
  Future<void> addTransferHistory(Map<String, dynamic> entry) async {
    final list = _prefs.getStringList('transferHistory') ?? [];
    list.insert(0, jsonEncode(entry));
    if (list.length > 100) list.removeLast();
    await _prefs.setStringList('transferHistory', list);
    notifyListeners();
  }
}
