import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'package:open_filex/open_filex.dart';
import 'package:share_plus/share_plus.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:mobile/src/rust/api/plenum_api.dart';
import 'package:permission_handler/permission_handler.dart';
import '../services/receive_storage.dart';
import '../services/internet_settings.dart';
import '../services/transfer_lock.dart';
import '../theme.dart';
import '../widgets/animated_radar.dart';
import 'package:provider/provider.dart';
import '../services/settings_service.dart';

import '../utils/transfer_status.dart';
import '../utils/formatters.dart';
import '../widgets/success_check.dart';
import 'settings_screen.dart';

class ReceiveScreen extends StatefulWidget {
  const ReceiveScreen({super.key});

  @override
  State<ReceiveScreen> createState() => _ReceiveScreenState();
}

class _ReceiveScreenState extends State<ReceiveScreen> {
  TransferMode _mode = TransferMode.local;

  bool _isListening = false;
  String _statusMessage = 'Tap radar to start receiving';
  String? _pin;
  double? _progress;
  bool _copied = false;

  String? _roomCode;
  bool _roomCodeCopied = false;
  bool _remoteStarted = false;

  StreamSubscription<String>? _localSub;
  StreamSubscription<String>? _remoteSub;
  String? _sessionToken;
  bool _requirePinActive = false;
  bool _autoAcceptActive = true;

  int? _totalBytes;
  int? _transferredBytes;
  DateTime? _transferStartTime;
  String? _speedText;
  String? _etaText;
  String? _savedFilePath;
  String? _savedLocation;
  Timer? _reArmTimer;

  // Summary fields kept for the success card after completion.
  String? _completedPeerName;
  String? _completedDuration;
  String? _completedMode;

  void _handleLogEvent(dynamic log) {
    final level = log['level'];
    if (level == 'Warn' || level == 'Error') {
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(
        content: Text(friendlyLogMessage(level, log['message'])),
        backgroundColor: level == 'Error' ? Colors.redAccent : Colors.orange,
      ));
    }
  }

  @override
  void dispose() {
    final active = _isListening || _remoteStarted;
    final token = _sessionToken;
    if (token != null && !active) {
      try {
        cancelSession(sessionToken: token);
      } catch (_) {}
    }
    // Clean up app-dir copy if it was already exported to Downloads.
    _deleteAppDirCopyIfExported();
    if (!active) {
      TransferLock.release();
      _localSub?.cancel();
      _remoteSub?.cancel();
    }
    _reArmTimer?.cancel();
    super.dispose();
  }

  /// Deletes the app-dir copy of the received file if it was successfully
  /// exported to public Downloads. The app-dir copy is only needed for
  /// Open/Share; once the user moves on, reclaim the space.
  void _deleteAppDirCopyIfExported() {
    final path = _savedFilePath;
    if (path != null && _savedLocation != null) {
      try {
        final f = File(path);
        if (f.existsSync()) f.deleteSync();
      } catch (_) {}
    }
  }

  void _stopReceiving() {
    // Cancel Rust-side transfer loop first, then drop the Dart subscriptions.
    TransferLock.release();
    final token = _sessionToken;
    if (token != null) {
      try {
        cancelSession(sessionToken: token);
      } catch (_) {}
    }
    _sessionToken = null;
    _localSub?.cancel();
    _remoteSub?.cancel();
    _reArmTimer?.cancel();
    setState(() {
      _isListening = false;
      _remoteStarted = false;
      _statusMessage = 'Tap radar to start receiving';
      _pin = null;
      _requirePinActive = false;
      _roomCode = null;
      _progress = null;
      _totalBytes = null;
      _transferredBytes = null;
      _transferStartTime = null;
      _speedText = null;
      _etaText = null;
      _savedFilePath = null;
      _savedLocation = null;
    });
  }

  Future<void> _startLocalReceiver() async {
    if (_isListening) return;

    final grantedStorage = await ReceiveStorage.ensurePermission();
    if (!grantedStorage) {
      if (mounted) {
        setState(() => _statusMessage = 'Storage permission needed to save files');
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(
          content: const Text('Storage permission is required to save files to Download'),
          action: SnackBarAction(label: 'Settings', onPressed: () => openAppSettings()),
        ));
      }
      return;
    }

    await Permission.nearbyWifiDevices.request();
    if (!mounted) return;

    final outputDir = await ReceiveStorage.outputDir();
    if (!mounted) return;
    final settings = context.read<SettingsService>();
    final deviceName = settings.deviceName;
    final requirePin = settings.requirePin;
    final autoAccept = settings.autoAccept;

    setState(() {
      _isListening = true;
      _requirePinActive = requirePin;
      _autoAcceptActive = autoAccept;
      _statusMessage = 'Listening for incoming files...';
    });

    try {
      await TransferLock.acquire();
    } catch (error) {
      if (mounted) {
        _handleLogEvent({
          'level': 'Warn',
          'message': 'Transfer lock unavailable; the transfer may pause when the screen is off: $error',
        });
      }
    }

    final sessionToken = DateTime.now().millisecondsSinceEpoch.toString();
    _sessionToken = sessionToken;
    _localSub = startReceive(
      outputDir: outputDir,
      port: 0,
      announce: true,
      deviceName: deviceName,
      sessionToken: sessionToken,
      requirePin: requirePin,
      autoAccept: autoAccept,
    ).listen((eventJson) {
      if (!mounted) return;
      final event = jsonDecode(eventJson);

      if (event['Log'] != null) {
        _handleLogEvent(event['Log']);
        return;
      }

      if (event['Discovery'] != null) {
        final discEvent = event['Discovery'];
        if (discEvent['BroadcastStarted'] != null) {
          setState(() {
            _pin = discEvent['BroadcastStarted']['token'];
            _statusMessage = 'Ready to receive files';
          });
        }
      } else if (event['Transfer'] != null) {
        _handleTransferEvent(event['Transfer'], outputDir);
      }
    }, onDone: () {
      TransferLock.release();
      if (mounted) setState(() => _isListening = false);
    }, onError: (e) {
      TransferLock.release();
      setState(() {
        _statusMessage = friendlyError(e);
        _isListening = false;
      });
    });
  }

  void _handleTransferEvent(dynamic transEvent, String outputDir) {
    if (transEvent['StateChanged'] != null) {
      final state = transEvent['StateChanged']['state'];
      if (state == 'Closed') {
        _stopReceiving();
        setState(() => _statusMessage = 'Connection closed');
      } else {
        setState(() {
          _statusMessage = state == 'Connected' ? 'Connected to device...' : friendlyState(state, isReceive: true);
        });
      }
    } else if (transEvent['IncomingRequest'] != null) {
      final req = transEvent['IncomingRequest'];
      final fileName = req['file_name'] ?? 'Unknown file';
      final totalBytes = req['total_bytes'] ?? 0;
      if (_autoAcceptActive) {
        // Auto-accept skips the dialog. Surface the file/sender explicitly so the
        // user has confirmation of what was accepted even if negotiation stalls
        // on a slow network.
        final sender = req['sender_name'] ?? req['peer'] ?? 'device';
        setState(() {
          _statusMessage = 'Accepting $fileName (${formatBytes(totalBytes)}) from $sender...';
        });
      } else {
        _showIncomingRequestDialog(
          fileName: fileName,
          totalBytes: totalBytes,
          peer: req['peer'],
          senderName: req['sender_name'],
        );
      }
    } else if (transEvent['Cancelled'] != null) {
      TransferLock.release();
      setState(() {
        _statusMessage = 'Transfer cancelled';
        _progress = null;
      });
    } else if (transEvent['Declined'] != null) {
      TransferLock.release();
      final reason = transEvent['Declined']['reason'];
      setState(() {
        _statusMessage = reason == 'cancelled'
            ? 'Sender cancelled the transfer'
            : 'Transfer declined';
        _progress = null;
      });
    } else if (transEvent['Started'] != null) {
      setState(() {
        _statusMessage = 'Receiving ${transEvent['Started']['file_name']}...';
        _progress = 0.0;
        _totalBytes = transEvent['Started']['total_bytes'];
        _transferStartTime = DateTime.now();
        _speedText = null;
        _etaText = null;
      });
    } else if (transEvent['Failed'] != null) {
      setState(() {
        _statusMessage = friendlyError(transEvent['Failed']['message']);
        _progress = null;
        _speedText = null;
        _etaText = null;
      });
    } else if (transEvent['Resumed'] != null) {
      setState(() {
        final resumedBytes = transEvent['Resumed']['resumed_bytes'] ?? 0;
        final percent = _totalBytes != null && _totalBytes! > 0 ? (resumedBytes / _totalBytes! * 100).toStringAsFixed(1) : '0';
        _statusMessage = 'Resuming from $percent%...';
      });
    } else if (transEvent['Progress'] != null) {
      final p = transEvent['Progress'];
      setState(() {
        _transferredBytes = p['transferred_bytes'];
        _totalBytes = p['total_bytes'];
        if (_totalBytes != null && _totalBytes! > 0) {
          // Clamp: a duplicate/late Progress event can report more than total.
          _progress = (_transferredBytes! / _totalBytes!).clamp(0.0, 1.0);
        }

        if (_transferStartTime != null && _transferredBytes != null && _totalBytes != null) {
          final elapsed = DateTime.now().difference(_transferStartTime!);
          if (elapsed.inSeconds > 0) {
            final speedBps = _transferredBytes! / elapsed.inSeconds;
            _speedText = '${formatBytes(speedBps.round())}/s';
            final remainingBytes = _totalBytes! - _transferredBytes!;
            final etaSeconds = speedBps > 0 ? remainingBytes / speedBps : 0;
            _etaText = '${etaSeconds.round()}s left';
          }
        }
      });
    } else if (transEvent['Completed'] != null) {
      TransferLock.release();
      final summary = transEvent['Completed'];
      final fileName = summary['file_name'];
      final localPath = fileName != null ? '$outputDir/$fileName' : null;
      final peerName = summary['peer_name'] ?? summary['peer'] ?? 'Unknown sender';
      final elapsedMs = summary['elapsed_ms'];
      final mode = formatTransferMode(summary['mode']);
      final settings = context.read<SettingsService>();
      settings.addTransferHistory({
        'direction': 'receive',
        'fileName': fileName ?? 'Unknown file',
        'size': summary['total_bytes'] ?? _totalBytes,
        'peerName': peerName,
        'durationMs': elapsedMs,
        'mode': summary['mode'],
        'path': localPath,
        'timestamp': DateTime.now().toIso8601String(),
      });

      // Build a rich status message with summary details.
      String statusMsg = 'Transfer complete!';
      if (elapsedMs != null) {
        statusMsg = 'Received from $peerName in ${formatDuration(elapsedMs)}';
      }

      setState(() {
        _statusMessage = statusMsg;
        _progress = 1.0;
        _savedFilePath = localPath;
        _savedLocation = null;
        _completedPeerName = peerName;
        _completedDuration = elapsedMs != null ? formatDuration(elapsedMs) : null;
        _completedMode = mode;
      });
      if (localPath != null) {
        ReceiveStorage.exportToDownloads(localPath).then((saved) {
          if (mounted && saved != null) {
            setState(() => _savedLocation = saved);
          }
        });
      }
      _reArmTimer?.cancel();
      _reArmTimer = Timer(const Duration(seconds: 8), () {
        if (mounted && _progress == 1.0) _reArmReceiving();
      });
    }
  }

  void _reArmReceiving() {
    final saved = _savedFilePath;
    final savedLoc = _savedLocation;
    // Delete the app-dir copy now that the user has had time to Open/Share.
    _deleteAppDirCopyIfExported();
    _stopReceiving();
    setState(() {
      _savedFilePath = saved;
      _savedLocation = savedLoc;
      _progress = 1.0; // keep the saved-file card visible
    });
    if (_mode == TransferMode.local) {
      _startLocalReceiver();
    } else {
      _setupRemoteReceiver();
    }
  }

  Future<void> _showIncomingRequestDialog({
    required String fileName,
    required int totalBytes,
    String? peer,
    String? senderName,
  }) async {
    if (_autoAcceptActive) return; // engine already proceeding
    final token = _sessionToken;
    if (token == null) return;
    setState(() => _statusMessage = 'Incoming file — waiting for your decision');
    final accepted = await showDialog<bool>(
      context: context,
      barrierDismissible: false,
      builder: (context) => AlertDialog(
        title: const Text('Incoming file'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(fileName, style: const TextStyle(fontWeight: FontWeight.bold)),
            const SizedBox(height: 4),
            Text(formatBytes(totalBytes), style: const TextStyle(color: AppTheme.textSecondary)),
            if (senderName != null) ...[
              const SizedBox(height: 8),
              Text('From: $senderName',
                  style: const TextStyle(color: AppTheme.textPrimary, fontWeight: FontWeight.w600, fontSize: 14)),
              if (peer != null) ...[
                const SizedBox(height: 2),
                Text(peer, style: const TextStyle(color: AppTheme.textSecondary, fontSize: 11)),
              ],
            ] else if (peer != null) ...[
              const SizedBox(height: 4),
              Text('From: $peer', style: const TextStyle(color: AppTheme.textSecondary, fontSize: 12)),
            ],
          ],
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('Decline'),
          ),
          ElevatedButton(
            onPressed: () => Navigator.of(context).pop(true),
            child: const Text('Accept'),
          ),
        ],
      ),
    );
    if (!mounted) return;
    try {
      respondToIncoming(sessionToken: token, accept: accepted == true);
    } catch (_) {}
    if (accepted != true) {
      setState(() => _statusMessage = 'Transfer declined');
    }
  }

  Future<void> _setupRemoteReceiver() async {
    _stopReceiving();
    if (_remoteStarted) return;
    _remoteStarted = true;

    final granted = await ReceiveStorage.ensurePermission();
    if (!granted) {
      if (mounted) {
        setState(() => _statusMessage = 'Storage permission needed to save files');
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(
          content: const Text('Storage permission is required to save files to Download'),
          action: SnackBarAction(label: 'Settings', onPressed: () => openAppSettings()),
        ));
      }
      _remoteStarted = false;
      return;
    }

    setState(() {
      _statusMessage = 'Generating room code...';
      _roomCode = null;
      _progress = null;
    });

    final code = generateRoomCodeSync();
    final myPeerId = generatePeerIdSync();

    if (!mounted) return;
    setState(() => _roomCode = code);

    final settings = context.read<SettingsService>();
    final relayServerUrl = settings.relayServerUrl;
    final autoAccept = settings.autoAccept;
    _autoAcceptActive = autoAccept;
    final iceServers = settings.iceServers
        .split('\n')
        .map((e) => e.trim())
        .where((e) => e.isNotEmpty)
        .map((e) => IceServerSetting(urls: e))
        .toList();

    if (mounted) setState(() => _statusMessage = 'Waiting for sender...');

    final outputDir = await ReceiveStorage.outputDir();
    final iceServersJson = await InternetSettings.buildIceServersJsonWithTurn(
      relayServerUrl,
      myPeerId,
      iceServers,
    );

    try {
      await TransferLock.acquire();
    } catch (error) {
      if (mounted) {
        _handleLogEvent({
          'level': 'Warn',
          'message': 'Transfer lock unavailable; the transfer may pause when the screen is off: $error',
        });
      }
    }

    final sessionToken = DateTime.now().millisecondsSinceEpoch.toString();
    _sessionToken = sessionToken;
    _remoteSub = startReceiveRemote(
      outputDir: outputDir,
      relayServerUrl: relayServerUrl,
      sessionId: code,
      myPeerId: myPeerId,
      iceServersJson: iceServersJson,
      connectTimeoutSecs: BigInt.from(600),
      sessionToken: sessionToken,
      autoAccept: autoAccept,
      deviceName: settings.deviceName,
    ).listen((eventJson) {
      if (!mounted) return;
      final event = jsonDecode(eventJson);
      if (event['Log'] != null) {
        _handleLogEvent(event['Log']);
        return;
      }
      if (event['Transfer'] != null) {
        _handleTransferEvent(event['Transfer'], outputDir);
      }
    }, onDone: () {
      TransferLock.release();
      if (mounted) setState(() => _remoteStarted = false);
    }, onError: (e) {
      TransferLock.release();
      if (mounted) {
        setState(() {
          _statusMessage = friendlyError(e);
          _remoteStarted = false;
        });
      }
    });
  }

  Future<void> _copyPin() async {
    if (_pin == null) return;
    try {
      await Clipboard.setData(ClipboardData(text: _pin!));
      if (!mounted) return;
      setState(() => _copied = true);
      Future.delayed(const Duration(seconds: 2), () {
        if (mounted) setState(() => _copied = false);
      });
    } catch (_) {
      // handle clipboard access denied
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          const SnackBar(content: Text('Could not copy — copy the code manually')),
        );
      }
    }
  }

  Future<void> _copyRoomCode() async {
    if (_roomCode == null) return;
    try {
      await Clipboard.setData(ClipboardData(text: _roomCode!));
      if (!mounted) return;
      setState(() => _roomCodeCopied = true);
      Future.delayed(const Duration(seconds: 2), () {
        if (mounted) setState(() => _roomCodeCopied = false);
      });
    } catch (_) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          const SnackBar(content: Text('Could not copy — copy the code manually')),
        );
      }
    }
  }

  void _showFileMissingSnack() {
    if (!mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(const SnackBar(
      content: Text('This file was moved or deleted and can no longer be opened.'),
    ));
  }

  String _openResultMessage(ResultType type) {
    switch (type) {
      case ResultType.noAppToOpen:
        return 'No app is available to open this file type';
      case ResultType.permissionDenied:
        return 'Permission denied while opening the file';
      case ResultType.fileNotFound:
        return 'This file was moved or deleted';
      default:
        return 'Could not open the file';
    }
  }

  Future<void> _openSavedFile() async {
    final path = _savedFilePath;
    if (path == null) return;
    if (!await File(path).exists()) {
      _showFileMissingSnack();
      return;
    }
    final result = await OpenFilex.open(path);
    if (result.type != ResultType.done && mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text(_openResultMessage(result.type))),
      );
    }
  }

  Future<void> _shareSavedFile() async {
    final path = _savedFilePath;
    if (path == null) return;
    if (!await File(path).exists()) {
      _showFileMissingSnack();
      return;
    }
    try {
      // ignore: deprecated_member_use
      await Share.shareXFiles([XFile(path)]);
    } catch (_) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          const SnackBar(content: Text('Could not share the file')),
        );
      }
    }
  }

  void _switchMode(TransferMode mode) {
    if (mode == _mode) return;
    _stopReceiving();
    setState(() {
      _mode = mode;
      _statusMessage = mode == TransferMode.local
          ? 'Tap radar to start receiving'
          : 'Preparing...';
    });
    if (mode == TransferMode.internet) {
      _setupRemoteReceiver();
    }
  }

  Widget _buildCodeCard({
    required String caption,
    required String code,
    required bool copied,
    required VoidCallback onCopy,
    VoidCallback? onShare,
    String? footer,
  }) {
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 12),
      decoration: BoxDecoration(
        color: AppTheme.bgCard,
        borderRadius: BorderRadius.circular(12),
        border: Border.all(color: AppTheme.accentPrimary),
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(
            caption,
            style: const TextStyle(fontSize: 12, color: AppTheme.textSecondary),
            textAlign: TextAlign.center,
            maxLines: 2,
            overflow: TextOverflow.ellipsis,
          ),
          const SizedBox(height: 8),
          Row(
            children: [
              Expanded(
                child: FittedBox(
                  fit: BoxFit.scaleDown,
                  alignment: Alignment.center,
                  child: Text(
                    code,
                    style: const TextStyle(
                      fontSize: 26,
                      fontWeight: FontWeight.bold,
                      letterSpacing: 4,
                      color: AppTheme.accentPrimary,
                    ),
                    maxLines: 1,
                  ),
                ),
              ),
              const SizedBox(width: 8),
              GestureDetector(
                onTap: onCopy,
                child: Container(
                  padding: const EdgeInsets.all(8),
                  decoration: BoxDecoration(
                    color: AppTheme.bgSidebar,
                    borderRadius: BorderRadius.circular(8),
                  ),
                  child: Icon(
                    copied ? Icons.check : Icons.copy,
                    size: 18,
                    color: copied ? AppTheme.accentPrimary : AppTheme.textSecondary,
                  ),
                ),
              ),
              if (onShare != null) ...[
                const SizedBox(width: 6),
                GestureDetector(
                  onTap: onShare,
                  child: Container(
                    padding: const EdgeInsets.all(8),
                    decoration: BoxDecoration(
                      color: AppTheme.bgSidebar,
                      borderRadius: BorderRadius.circular(8),
                    ),
                    child: const Icon(Icons.share, size: 18, color: AppTheme.textSecondary),
                  ),
                ),
              ],
            ],
          ),
          if (footer != null) ...[
            const SizedBox(height: 8),
            Text(
              footer,
              style: const TextStyle(fontSize: 12, color: AppTheme.textSecondary),
              textAlign: TextAlign.center,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
            ),
          ],
        ],
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      resizeToAvoidBottomInset: false,
      appBar: AppBar(
        title: const Text('Plenum', style: TextStyle(fontWeight: FontWeight.w900, color: AppTheme.accentPrimary, letterSpacing: -0.5)),
        actions: [
          IconButton(
            icon: const Icon(Icons.settings),
            onPressed: () {
              Navigator.push(
                context,
                MaterialPageRoute(builder: (context) => const SettingsScreen()),
              );
            },
          )
        ],
      ),
      body: Padding(
        padding: const EdgeInsets.fromLTRB(16, 8, 16, 8),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Row(
              children: [
                Expanded(
                  child: _ModeChip(
                    icon: Icons.wifi,
                    label: 'Local Network',
                    selected: _mode == TransferMode.local,
                    onTap: () => _switchMode(TransferMode.local),
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: _ModeChip(
                    icon: Icons.public,
                    label: 'Internet',
                    selected: _mode == TransferMode.internet,
                    onTap: () => _switchMode(TransferMode.internet),
                  ),
                ),
              ],
            ),
            Expanded(
              child: Column(
                mainAxisAlignment: MainAxisAlignment.center,
                children: [
                  GestureDetector(
                    onTap: _mode == TransferMode.local
                        ? (_isListening ? null : _startLocalReceiver)
                        : (_remoteStarted ? null : _setupRemoteReceiver),
                    child: AnimatedRadar(
                      isListening: _mode == TransferMode.local ? _isListening : _remoteStarted,
                    ),
                  ),
                  const SizedBox(height: 16),
                  Text(
                    _statusMessage,
                    style: const TextStyle(fontSize: 15, color: AppTheme.textSecondary),
                    textAlign: TextAlign.center,
                    maxLines: 2,
                    overflow: TextOverflow.ellipsis,
                  ),
                  if (_isListening || _remoteStarted)
                    TextButton.icon(
                      onPressed: _stopReceiving,
                      style: TextButton.styleFrom(
                        visualDensity: VisualDensity.compact,
                        tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                      ),
                      icon: const Icon(Icons.stop_circle, color: AppTheme.accentPrimary, size: 18),
                      label: const Text('Stop Receiving', style: TextStyle(color: AppTheme.accentPrimary)),
                    ),
                  if (isTransferFailure(_statusMessage))
                    Padding(
                      padding: const EdgeInsets.only(top: 4),
                      child: ElevatedButton.icon(
                        onPressed: () {
                          _stopReceiving();
                          if (_mode == TransferMode.local) {
                            _startLocalReceiver();
                          } else {
                            _setupRemoteReceiver();
                          }
                        },
                        icon: const Icon(Icons.refresh),
                        label: const Text('Retry'),
                      ),
                    ),
                ],
              ),
            ),
            if (_mode == TransferMode.local && _pin != null)
              _buildCodeCard(
                caption: _requirePinActive
                    ? 'PIN required senders must enter this code'
                    : 'Pairing code - senders can use this to find you',
                code: _pin!,
                copied: _copied,
                onCopy: _copyPin,

              ),
            if (_mode == TransferMode.internet && _roomCode != null)
              _buildCodeCard(
                caption: 'Room Code',
                code: _roomCode!,
                copied: _roomCodeCopied,
                onCopy: _copyRoomCode,
                // ignore: deprecated_member_use
                onShare: () => Share.share('Use this code to send files on Plenum: $_roomCode'),
                footer: 'Code valid while this screen is open',
              ),
            if (_progress != null) ...[
              const SizedBox(height: 8),
              Container(
                width: double.infinity,
                padding: const EdgeInsets.all(12),
                decoration: BoxDecoration(
                  color: AppTheme.bgSidebar,
                  borderRadius: BorderRadius.circular(12),
                ),
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    ClipRRect(
                      borderRadius: BorderRadius.circular(4),
                      child: LinearProgressIndicator(
                        value: _progress,
                        minHeight: 8,
                        backgroundColor: AppTheme.bgApp,
                        valueColor: const AlwaysStoppedAnimation<Color>(AppTheme.accentPrimary),
                      ),
                    ),
                    const SizedBox(height: 6),
                    Row(
                      children: [
                        Text(
                          '${(_progress! * 100).toStringAsFixed(1)}%  •  ${formatBytes(_transferredBytes ?? 0)} / ${formatBytes(_totalBytes ?? 0)}',
                          style: const TextStyle(color: AppTheme.textSecondary, fontSize: 12),
                        ),
                        if (_speedText != null && _etaText != null) ...[
                          const Spacer(),
                          Flexible(
                            child: Text(
                              '$_speedText • $_etaText',
                              style: const TextStyle(color: AppTheme.textSecondary, fontSize: 12),
                              overflow: TextOverflow.ellipsis,
                              textAlign: TextAlign.end,
                            ),
                          ),
                        ],
                      ],
                    ),
                    if (_progress == 1.0 && _savedFilePath != null) ...[
                      const SizedBox(height: 8),
                      const Center(child: SuccessCheck()),
                      const SizedBox(height: 12),
                      Container(
                        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
                        decoration: BoxDecoration(
                          color: AppTheme.bgCard,
                          borderRadius: BorderRadius.circular(8),
                          border: Border.all(color: AppTheme.borderColor),
                        ),
                        child: Column(
                          children: [
                            if (_completedPeerName != null) ...[
                              Row(
                                children: [
                                  const Icon(Icons.monitor, size: 16, color: AppTheme.textSecondary),
                                  const SizedBox(width: 8),
                                  Expanded(child: Text(_completedPeerName!, style: const TextStyle(color: AppTheme.textSecondary, fontSize: 13))),
                                ],
                              ),
                              const SizedBox(height: 8),
                            ],
                            Row(
                              children: [
                                const Icon(Icons.timer_outlined, size: 16, color: AppTheme.textSecondary),
                                const SizedBox(width: 8),
                                Expanded(
                                  child: Text(
                                    '${_completedDuration ?? "-"} • ${_completedMode ?? "Unknown"}',
                                    style: const TextStyle(color: AppTheme.textSecondary, fontSize: 13),
                                  ),
                                ),
                              ],
                            ),
                          ],
                        ),
                      ),
                      const SizedBox(height: 8),
                      Container(
                        width: double.infinity,
                        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
                        decoration: BoxDecoration(
                          color: AppTheme.accentPrimary.withValues(alpha: 0.1),
                          borderRadius: BorderRadius.circular(8),
                          border: Border.all(color: AppTheme.accentPrimary.withValues(alpha: 0.4)),
                        ),
                        child: Text(
                          _savedLocation != null ? 'Saved to $_savedLocation' : 'Saving to Downloads...',
                          style: const TextStyle(color: AppTheme.textPrimary, fontSize: 12, fontWeight: FontWeight.w600),
                          textAlign: TextAlign.center,
                          maxLines: 2,
                          overflow: TextOverflow.ellipsis,
                        ),
                      ),
                      const SizedBox(height: 12),
                      Row(
                        children: [
                          Expanded(
                            child: ElevatedButton(
                              onPressed: _openSavedFile,
                              child: const Text('Open'),
                            ),
                          ),
                          const SizedBox(width: 8),
                          Expanded(
                            child: ElevatedButton(
                              onPressed: _shareSavedFile,
                              child: const Text('Share'),
                            ),
                          ),
                        ],
                      ),
                      TextButton(
                        onPressed: () {
                          _stopReceiving();
                          if (_mode == TransferMode.local) {
                            _startLocalReceiver();
                          } else {
                            _setupRemoteReceiver();
                          }
                        },
                        style: TextButton.styleFrom(
                          visualDensity: VisualDensity.compact,
                          tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                        ),
                        child: const Text('Receive another'),
                      ),
                    ],
                  ],
                ),
              ),
            ],
          ],
        ),
      ),
    );
  }
}

class _ModeChip extends StatelessWidget {
  final IconData icon;
  final String label;
  final bool selected;
  final VoidCallback onTap;

  const _ModeChip({required this.icon, required this.label, required this.selected, required this.onTap});

  @override
  Widget build(BuildContext context) {
    return GestureDetector(
      onTap: onTap,
      child: Container(
        width: double.infinity,
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 10),
        decoration: BoxDecoration(
          color: AppTheme.bgCard,
          borderRadius: BorderRadius.circular(20),
          border: Border.all(color: selected ? AppTheme.accentPrimary : AppTheme.borderColor, width: selected ? 2 : 1),
        ),
        child: Row(
          mainAxisAlignment: MainAxisAlignment.center,
          children: [
            Icon(icon, size: 16, color: selected ? AppTheme.accentPrimary : AppTheme.textSecondary),
            const SizedBox(width: 6),
            Flexible(
              child: Text(
                label,
                style: TextStyle(
                  color: selected ? AppTheme.accentPrimary : AppTheme.textSecondary,
                  fontWeight: FontWeight.w600,
                  fontSize: 12,
                ),
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
              ),
            ),
          ],
        ),
      ),
    );
  }
}
