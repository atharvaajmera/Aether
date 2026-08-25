import 'dart:async';
import 'dart:convert';
import 'package:flutter/material.dart';
import 'package:file_picker/file_picker.dart';
import 'package:permission_handler/permission_handler.dart';
import 'package:mobile/src/rust/api/plenum_api.dart';
import '../services/internet_settings.dart';
import '../theme.dart';
import 'package:provider/provider.dart';
import '../services/settings_service.dart';
import '../services/transfer_lock.dart';

import '../utils/transfer_status.dart';
import '../utils/formatters.dart';
import '../widgets/success_check.dart';
import 'settings_screen.dart';

class SendScreen extends StatefulWidget {
  const SendScreen({super.key});

  @override
  State<SendScreen> createState() => _SendScreenState();
}

class _SendScreenState extends State<SendScreen> {
  TransferMode _mode = TransferMode.local;
  String? _selectedFile;
  final List<Map<String, dynamic>> _peers = [];
  bool _isDiscovering = false;
  String _transferStatus = '';
  String? _currentTransferPeerName;
  double? _progress;
  int? _selectedFileSize;

  int? _totalBytes;
  int? _transferredBytes;
  DateTime? _transferStartTime;
  String? _speedText;
  String? _etaText;

  final TextEditingController _roomCodeController = TextEditingController();
  bool _isConnectingRemote = false;
  String? _sessionToken;
  /// Ownership token for the currently-held [TransferLock], if any.
  Object? _lockToken;
  StreamSubscription<String>? _transferSub;
  StreamSubscription<String>? _discoverySub;
  bool _transferActive = false;
  bool _showSuccess = false;
  Timer? _autoResetTimer;
  
  String? _completedFileName;
  String? _completedPeerName;
  String? _completedDuration;
  String? _completedMode;

  @override
  void initState() {
    super.initState();
    _startDiscovery();
  }

  @override
  void dispose() {
    final token = _sessionToken;
    if (token != null && !_transferActive) {
      try {
        cancelSession(sessionToken: token);
      } catch (_) {}
    }
    if (!_transferActive) _transferSub?.cancel();
    _discoverySub?.cancel();
    _autoResetTimer?.cancel();
    _roomCodeController.dispose();
    if (!_transferActive) {
      TransferLock.release(_lockToken);
      _lockToken = null;
    }
    super.dispose();
  }

  /// Cancels an in-flight transfer: flips the Rust-side flag; the engine
  /// sends `Close` to the peer, emits `Cancelled`, and returns.
  void _cancelTransfer() {
    final token = _sessionToken;
    if (token != null) {
      try {
        cancelSession(sessionToken: token);
      } catch (_) {}
    }
  }

  void _resetTransferUi() {
    _autoResetTimer?.cancel();
    setState(() {
      _transferStatus = '';
      _progress = null;
      _totalBytes = null;
      _transferredBytes = null;
      _transferStartTime = null;
      _speedText = null;
      _etaText = null;
      _showSuccess = false;
      _completedFileName = null;
      _completedPeerName = null;
      _completedDuration = null;
      _completedMode = null;
    });
  }

  void _startDiscovery() async {
    _discoverySub?.cancel();
    _discoverySub = null;

    await Permission.nearbyWifiDevices.request();
    if (!mounted) return;

    setState(() {
      _peers.clear();
      _transferStatus = '';
      _progress = null;
    });

    _discoverySub = startDiscovery(timeoutSecs: BigInt.from(10)).listen((eventJson) {
      if (!mounted) return;
      final event = jsonDecode(eventJson);
      if (event['Discovery'] != null) {
        final discEvent = event['Discovery'];
        if (discEvent == 'PeerNotFound') {
          setState(() {
            _isDiscovering = false;
            _transferStatus = 'No devices found';
          });
        } else if (discEvent is Map) {
          if (discEvent['PeerFound'] != null) {
            setState(() {
              final found = discEvent['PeerFound'];
              final token = found['token'];
              final duplicate = _peers.any((p) =>
                  p['address'] == found['address'] ||
                  (token != null && token.toString().isNotEmpty && p['token'] == token));
              if (!duplicate) {
                _peers.add(found);
              }
            });
          } else if (discEvent['SearchStarted'] != null) {
            setState(() {
              _isDiscovering = true;
            });
          }
        }
      }
    }, onDone: () {
      if (mounted) setState(() => _isDiscovering = false);
    });
  }

  Future<void> _pickFile() async {
    FilePickerResult? result = await FilePicker.pickFiles();
    if (result != null) {
      setState(() {
        _selectedFile = result.files.single.path;
        _selectedFileSize = result.files.single.size;
      });
    }
  }

  void _handleTransferEvent(String eventJson) {
    if (!mounted) return;
    final event = jsonDecode(eventJson);
    if (event['Log'] != null) {
      final log = event['Log'];
      final level = log['level'];
      if (level == 'Warn' || level == 'Error') {
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(
          content: Text(friendlyLogMessage(level, log['message'])),
          backgroundColor: level == 'Error' ? Colors.redAccent : Colors.orange,
        ));
      }
      return;
    }
    if (event['Transfer'] != null) {
      final trans = event['Transfer'];
      if (trans['StateChanged'] != null) {
        final state = trans['StateChanged']['state'];
        if (state == 'Closed') {
          if (!_showSuccess) {
            setState(() {
              _transferStatus = '';
              _progress = null;
              _totalBytes = null;
              _transferredBytes = null;
              _transferStartTime = null;
              _speedText = null;
              _etaText = null;
              _isConnectingRemote = false;
              _transferActive = false;
            });
          }
        } else {
          setState(() => _transferStatus = friendlyState(state));
        }
      } else if (trans['AwaitingApproval'] != null) {
        setState(() {
          _transferStatus = 'Waiting for the receiver to accept...';
          _transferActive = true;
        });
      } else if (trans['Cancelled'] != null) {
        setState(() {
          _transferStatus = 'Transfer cancelled';
          _progress = null;
          _isConnectingRemote = false;
          _transferActive = false;
        });
      } else if (trans['Declined'] != null) {
        final reason = trans['Declined']['reason'];
        setState(() {
          _transferStatus = switch (reason) {
            'pin_rejected' => 'Wrong pairing code — check the code on the receiver\'s screen',
            'cancelled' => 'The receiver cancelled the transfer',
            _ => 'The receiver declined the transfer',
          };
          _progress = null;
          _isConnectingRemote = false;
          _transferActive = false;
        });
      } else if (trans['Failed'] != null) {
        setState(() {
          _transferStatus = friendlyError(trans['Failed']['message']);
          _progress = null;
          _isConnectingRemote = false;
          _transferActive = false;
        });
      } else if (trans['Started'] != null) {
        setState(() {
          _transferStatus = 'Sending ${trans['Started']['file_name']}...';
          _progress = 0.0;
          _totalBytes = trans['Started']['total_bytes'];
          _transferStartTime = DateTime.now();
          _speedText = null;
          _etaText = null;
          _transferActive = true;
        });
      } else if (trans['Resumed'] != null) {
        setState(() {
          final resumedBytes = trans['Resumed']['resumed_bytes'] ?? 0;
          final percent = _totalBytes != null && _totalBytes! > 0 ? (resumedBytes / _totalBytes! * 100).toStringAsFixed(1) : '0';
          _transferStatus = 'Resuming from $percent%...';
        });
      } else if (trans['Progress'] != null) {
        setState(() {
          _transferredBytes = trans['Progress']['transferred_bytes'];
          _totalBytes = trans['Progress']['total_bytes'];
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
      } else if (trans['Completed'] != null) {
        final summary = trans['Completed'];
        final settings = context.read<SettingsService>();
        final peerName = summary['peer_name'] ??
            summary['peer'] ??
            _currentTransferPeerName ??
            'Unknown device';
        final elapsedMs = summary['elapsed_ms'];
        final mode = formatTransferMode(summary['mode']);
        settings.addTransferHistory({
          'direction': 'send',
          'fileName': summary['file_name'] ?? _selectedFile?.split(RegExp(r'[\\/]')).last ?? 'Unknown file',
          'size': summary['total_bytes'] ?? _selectedFileSize,
          'peerName': peerName,
          'durationMs': elapsedMs,
          'mode': summary['mode'],
          'timestamp': DateTime.now().toIso8601String(),
        });
        setState(() {
          _transferStatus = elapsedMs != null
              ? 'Sent to $peerName in ${formatDuration(elapsedMs)}'
              : 'Sent to $peerName';
          _progress = 1.0;
          _showSuccess = true;
          _isConnectingRemote = false;
          _transferActive = false;
          _completedFileName = summary['file_name'] ?? _selectedFile?.split(RegExp(r'[\\/]')).last ?? 'Unknown file';
          _completedPeerName = peerName;
          _completedDuration = elapsedMs != null ? formatDuration(elapsedMs) : null;
          _completedMode = mode;
        });
        _autoResetTimer?.cancel();
        _autoResetTimer = Timer(const Duration(seconds: 8), () {
          if (mounted && _showSuccess) _resetTransferUi();
        });
      }
    }
  }

  Future<void> _sendToPeer(String address, String hostname, String? pin) async {
    if (_selectedFile == null) return;
    _currentTransferPeerName = hostname;

    final sessionToken = DateTime.now().millisecondsSinceEpoch.toString();
    _sessionToken = sessionToken;
    _autoResetTimer?.cancel();
    setState(() {
      _transferActive = true;
      _showSuccess = false;
    });
    Object? lockToken;
    try {
      lockToken = await TransferLock.acquire();
      _lockToken = lockToken;
    } catch (error) {
      if (mounted) {
        setState(() {
          _transferStatus = 'Transfer lock unavailable';
          _transferActive = false;
        });
      }
      return;
    }
    _transferSub = startSend(
      filePath: _selectedFile!,
      peerAddress: address,
      optionalPin: pin,
      deviceName: context.read<SettingsService>().deviceName,
      sessionToken: sessionToken,
    ).listen(
      _handleTransferEvent,
      onDone: () {
        TransferLock.release(lockToken);
        _lockToken = null;
        if (mounted) setState(() => _transferActive = false);
      },
      onError: (e) {
        TransferLock.release(lockToken);
        _lockToken = null;
        if (mounted) {
          setState(() {
            _transferStatus = friendlyError(e);
            _progress = null;
            _transferActive = false;
          });
        }
      },
    );
  }

  Future<void> _handleRoomCodeConnect() async {
    if (_selectedFile == null) {
      ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('Please select a file first')));
      return;
    }
    final roomCode = _roomCodeController.text.trim().toUpperCase();
    if (roomCode.isEmpty) {
      ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('Please enter a room code')));
      return;
    }
    if (!RegExp(r'^[A-Z0-9]{9}$').hasMatch(roomCode)) {
      ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('Room codes are 9 letters or numbers')));
      return;
    }
    if (_isConnectingRemote) return;

    final settings = context.read<SettingsService>();
    final relayServerUrl = settings.relayServerUrl;
    final iceServers = settings.iceServers
        .split('\n')
        .map((e) => e.trim())
        .where((e) => e.isNotEmpty)
        .map((e) => IceServerSetting(urls: e))
        .toList();
    _currentTransferPeerName = 'Remote Device ($roomCode)';

    setState(() {
      _transferStatus = 'Connecting to relay...';
      _isConnectingRemote = true;
    });

    Object? lockToken;
    try {
      final exists = await InternetSettings.roomExists(relayServerUrl, roomCode);
      if (!exists) {
        if (mounted) {
          setState(() {
            _transferStatus = 'Room not found. Ask the receiver to open Internet mode and share a new room code.';
            _isConnectingRemote = false;
          });
        }
        return;
      }

      final myPeerId = generatePeerIdSync();
      final iceServersJson = await InternetSettings.buildIceServersJsonWithTurn(
        relayServerUrl,
        myPeerId,
        iceServers,
      );
      final sessionToken = DateTime.now().millisecondsSinceEpoch.toString();
      _sessionToken = sessionToken;
      _autoResetTimer?.cancel();
      setState(() {
        _transferActive = true;
        _showSuccess = false;
      });
      lockToken = await TransferLock.acquire();
      _lockToken = lockToken;
      _transferSub = startSendRemote(
        filePath: _selectedFile!,
        relayServerUrl: relayServerUrl,
        sessionId: roomCode,
        myPeerId: myPeerId,
        iceServersJson: iceServersJson,
        connectTimeoutSecs: BigInt.from(30),
        deviceName: settings.deviceName,
        sessionToken: sessionToken,
      ).listen(
        _handleTransferEvent,
        onDone: () {
          TransferLock.release(lockToken);
          _lockToken = null;
          if (mounted) {
            setState(() {
              _isConnectingRemote = false;
              _transferActive = false;
            });
          }
        },
        onError: (e) {
          TransferLock.release(lockToken);
          _lockToken = null;
          if (mounted) {
            setState(() {
              _transferStatus = friendlyError(e);
              _progress = null;
              _isConnectingRemote = false;
              _transferActive = false;
            });
          }
        },
      );
    } catch (e) {
      TransferLock.release(lockToken);
      if (identical(_lockToken, lockToken)) _lockToken = null;
      setState(() {
        _transferStatus = friendlyError(e);
        _isConnectingRemote = false;
      });
    }
  }

  void _showPinDialog(String address, String hostname, {bool pinRequired = false}) {
    if (_selectedFile == null) return;

    final TextEditingController pinController = TextEditingController();

    void submit(BuildContext dialogContext) {
      final pin = pinController.text.trim();
      if (pinRequired && pin.isEmpty) return; // must enter a code
      Navigator.pop(dialogContext);
      unawaited(_sendToPeer(address, hostname, pin.isNotEmpty ? pin : null));
    }

    showDialog(
      context: context,
      builder: (context) {
        return AlertDialog(
          backgroundColor: AppTheme.bgCard,
          title: Text('Send to $hostname', style: const TextStyle(color: AppTheme.textPrimary)),
          content: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(
                pinRequired
                    ? 'This device requires a pairing code. Enter the code shown on its screen.'
                    : 'If the receiver requires a pairing code, enter it below. Otherwise, leave blank.',
                style: const TextStyle(color: AppTheme.textSecondary, fontSize: 14),
              ),
              const SizedBox(height: 16),
              TextField(
                controller: pinController,
                autofocus: true,
                textCapitalization: TextCapitalization.characters,
                decoration: InputDecoration(
                  labelText: pinRequired ? 'Pairing Code' : 'Pairing Code (Optional)',
                  border: const OutlineInputBorder(),
                  focusedBorder: const OutlineInputBorder(borderSide: BorderSide(color: AppTheme.accentPrimary)),
                ),
                style: const TextStyle(color: AppTheme.textPrimary),
                onSubmitted: (_) => submit(context),
              ),
            ],
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context),
              child: const Text('Cancel', style: TextStyle(color: AppTheme.textSecondary)),
            ),
            ElevatedButton(
              onPressed: () => submit(context),
              style: ElevatedButton.styleFrom(backgroundColor: AppTheme.accentPrimary),
              child: const Text('Send'),
            ),
          ],
        );
      }
    );
  }


  Widget _buildModeToggle() {
    return Row(
      children: [
        Expanded(
          child: _ModeCard(
            icon: Icons.wifi,
            label: 'Local Network',
            selected: _mode == TransferMode.local,
            onTap: () => setState(() => _mode = TransferMode.local),
          ),
        ),
        const SizedBox(width: 12),
        Expanded(
          child: _ModeCard(
            icon: Icons.public,
            label: 'Internet',
            selected: _mode == TransferMode.internet,
            onTap: () => setState(() => _mode = TransferMode.internet),
          ),
        ),
      ],
    );
  }

  Widget _buildFilePicker() {
    if (_selectedFile != null) {
      return Container(
        width: double.infinity,
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
        decoration: BoxDecoration(
          color: AppTheme.bgCard,
          borderRadius: BorderRadius.circular(12),
          border: Border.all(color: AppTheme.accentPrimary),
        ),
        child: Row(
          children: [
            const Icon(Icons.insert_drive_file, color: AppTheme.accentPrimary, size: 28),
            const SizedBox(width: 12),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    _selectedFile!.split(RegExp(r'[\\/]')).last,
                    style: const TextStyle(fontWeight: FontWeight.w600, color: AppTheme.textPrimary, fontSize: 14),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                  ),
                  if (_selectedFileSize != null) ...[
                    const SizedBox(height: 2),
                    Text(formatBytes(_selectedFileSize!), style: const TextStyle(color: AppTheme.textSecondary, fontSize: 12)),
                  ],
                ],
              ),
            ),
            IconButton(
              visualDensity: VisualDensity.compact,
              icon: const Icon(Icons.close, color: AppTheme.textSecondary),
              onPressed: () {
                setState(() {
                  _selectedFile = null;
                  _selectedFileSize = null;
                });
              },
            ),
          ],
        ),
      );
    }

    return GestureDetector(
      onTap: _pickFile,
      child: Container(
        width: double.infinity,
        padding: const EdgeInsets.symmetric(vertical: 14),
        decoration: BoxDecoration(
          gradient: const LinearGradient(
            begin: Alignment.topLeft,
            end: Alignment.bottomRight,
            colors: [AppTheme.bgCard, Color(0xFF1E2835)],
          ),
          borderRadius: BorderRadius.circular(12),
          border: Border.all(color: AppTheme.borderColor),
        ),
        child: const Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(Icons.upload_file, size: 32, color: AppTheme.textSecondary),
            SizedBox(height: 8),
            Text(
              'Select File to Send',
              style: TextStyle(fontWeight: FontWeight.w600, fontSize: 14, color: AppTheme.textPrimary),
            ),
          ],
        ),
      ),
    );
  }

  Widget _buildStatusCard() {
    if (_transferStatus.isEmpty) return const SizedBox.shrink();
    if (_showSuccess) {
      return Container(
        margin: const EdgeInsets.only(top: 8),
        padding: const EdgeInsets.all(16),
        decoration: BoxDecoration(
          color: AppTheme.bgSidebar,
          borderRadius: BorderRadius.circular(16),
          border: Border.all(color: AppTheme.accentPrimary.withValues(alpha: 0.3)),
        ),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Center(child: SuccessCheck()),
            const SizedBox(height: 12),
            Text(
              'Sent ${_completedFileName ?? "file"}',
              style: const TextStyle(color: AppTheme.textPrimary, fontSize: 16, fontWeight: FontWeight.bold),
              textAlign: TextAlign.center,
              maxLines: 2,
              overflow: TextOverflow.ellipsis,
            ),
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
            const SizedBox(height: 16),
            SizedBox(
              width: double.infinity,
              child: ElevatedButton(
                onPressed: _resetTransferUi,
                style: ElevatedButton.styleFrom(
                  backgroundColor: AppTheme.accentPrimary,
                  padding: const EdgeInsets.symmetric(vertical: 12),
                ),
                child: const Text('Send Another File', style: TextStyle(fontWeight: FontWeight.w600)),
              ),
            ),
          ],
        ),
      );
    }

    return Container(
      margin: const EdgeInsets.only(top: 8),
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
      decoration: BoxDecoration(
        color: AppTheme.bgSidebar,
        borderRadius: BorderRadius.circular(12),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(
            _transferStatus,
            style: const TextStyle(color: AppTheme.textPrimary, fontSize: 13),
            textAlign: TextAlign.center,
            maxLines: 3,
            overflow: TextOverflow.ellipsis,
          ),
          if (_isConnectingRemote) ...[
            const SizedBox(height: 8),
            const Center(child: SizedBox(width: 20, height: 20, child: CircularProgressIndicator(strokeWidth: 2, color: AppTheme.accentPrimary))),
          ],
          if (_progress != null) ...[
            const SizedBox(height: 8),
            ClipRRect(
              borderRadius: BorderRadius.circular(4),
              child: LinearProgressIndicator(
                value: _progress,
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
          ],
          if (_transferActive && _progress != 1.0)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: TextButton.icon(
                onPressed: _cancelTransfer,
                style: TextButton.styleFrom(
                  visualDensity: VisualDensity.compact,
                  tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                ),
                icon: const Icon(Icons.cancel, color: AppTheme.accentPrimary, size: 18),
                label: const Text('Cancel transfer', style: TextStyle(color: AppTheme.accentPrimary)),
              ),
            ),
          if (_progress == 1.0)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: ElevatedButton(
                onPressed: _resetTransferUi,
                child: const Text('Send another file'),
              ),
            )
        ],
      ),
    );
  }

  Widget _buildInternetPanel() {
    return Expanded(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          const Text('Connect via room code', style: TextStyle(fontWeight: FontWeight.w600, color: AppTheme.textPrimary, fontSize: 14)),
          const SizedBox(height: 12),
          Row(
            children: [
              Expanded(
                child: TextField(
                  controller: _roomCodeController,
                  textCapitalization: TextCapitalization.characters,
                  decoration: const InputDecoration(
                    isDense: true,
                    hintText: 'Enter room code',
                    border: OutlineInputBorder(),
                    focusedBorder: OutlineInputBorder(borderSide: BorderSide(color: AppTheme.accentPrimary)),
                    contentPadding: EdgeInsets.symmetric(horizontal: 12, vertical: 12),
                  ),
                  style: const TextStyle(color: AppTheme.textPrimary, letterSpacing: 2),
                  onSubmitted: (_) => _handleRoomCodeConnect(),
                ),
              ),
              const SizedBox(width: 8),
              ElevatedButton(
                onPressed: _isConnectingRemote ? null : _handleRoomCodeConnect,
                child: const Text('Connect'),
              ),
            ],
          ),
          _buildStatusCard(),
          const Spacer(),
          const Text(
            'Ask the receiver for their room code, then tap Connect to send over the internet.',
            style: TextStyle(fontSize: 12, color: AppTheme.textSecondary),
            textAlign: TextAlign.center,
          ),
        ],
      ),
    );
  }

  Widget _buildLocalPanel() {
    return Expanded(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            children: [
              const Expanded(
                child: Text('Discovered Devices', style: TextStyle(fontWeight: FontWeight.w600, color: AppTheme.textPrimary, fontSize: 14)),
              ),
              IconButton(
                visualDensity: VisualDensity.compact,
                padding: EdgeInsets.zero,
                constraints: const BoxConstraints(minWidth: 36, minHeight: 36),
                icon: _isDiscovering
                    ? const SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2, color: AppTheme.accentPrimary))
                    : const Icon(Icons.refresh, color: AppTheme.accentPrimary),
                onPressed: _isDiscovering ? null : _startDiscovery,
              ),
            ],
          ),
          Expanded(
            child: _peers.isEmpty
                ? (_isDiscovering
                    ? const Center(child: CircularProgressIndicator(color: AppTheme.accentPrimary))
                    : const Center(
                        child: Text(
                          'No devices found.',
                          textAlign: TextAlign.center,
                          style: TextStyle(color: AppTheme.textSecondary),
                        ),
                      ))
                : ListView.separated(
                    padding: EdgeInsets.zero,
                    itemCount: _peers.length,
                    separatorBuilder: (context, index) => const SizedBox(height: 8),
                    itemBuilder: (context, index) {
                      final peer = _peers[index];
                      return Container(
                        decoration: BoxDecoration(
                          color: AppTheme.bgCard,
                          borderRadius: BorderRadius.circular(12),
                          border: Border.all(color: AppTheme.borderColor),
                        ),
                        child: ListTile(
                          dense: true,
                          contentPadding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
                          onTap: () {
                            if (_selectedFile == null) {
                              ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('Please select a file first')));
                              return;
                            }
                            _showPinDialog(
                              peer['address'],
                              peer['hostname'] ?? 'Unknown Device',
                              pinRequired: peer['pin_required'] == true,
                            );
                          },
                          leading: Container(
                            padding: const EdgeInsets.all(8),
                            decoration: BoxDecoration(
                              color: AppTheme.bgSidebar,
                              borderRadius: BorderRadius.circular(8),
                            ),
                            child: const Icon(Icons.computer, color: AppTheme.accentPrimary, size: 22),
                          ),
                          title: Text(
                            peer['hostname'] ?? 'Unknown Device',
                            style: const TextStyle(fontWeight: FontWeight.w600, color: AppTheme.textPrimary),
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                          ),
                          subtitle: Text(
                            peer['address'] ?? '',
                            style: const TextStyle(color: AppTheme.textSecondary, fontSize: 12),
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                          ),
                          trailing: const Icon(Icons.send_rounded, color: AppTheme.accentPrimary),
                        ),
                      );
                    },
                  ),
          ),
          _buildStatusCard(),
          const SizedBox(height: 6),
          const Text(
            'Please ensure that the desired target is also on the same Wi-Fi network.',
            style: TextStyle(fontSize: 11, color: AppTheme.textSecondary, height: 1.3),
            textAlign: TextAlign.center,
            maxLines: 2,
            overflow: TextOverflow.ellipsis,
          ),
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
            _buildModeToggle(),
            const SizedBox(height: 10),
            _buildFilePicker(),
            const SizedBox(height: 10),
            _mode == TransferMode.local ? _buildLocalPanel() : _buildInternetPanel(),
          ],
        ),
      ),
    );
  }
}

class _ModeCard extends StatelessWidget {
  final IconData icon;
  final String label;
  final bool selected;
  final VoidCallback onTap;

  const _ModeCard({required this.icon, required this.label, required this.selected, required this.onTap});

  @override
  Widget build(BuildContext context) {
    return GestureDetector(
      onTap: onTap,
      child: Container(
        padding: const EdgeInsets.symmetric(vertical: 12),
        decoration: BoxDecoration(
          color: AppTheme.bgCard,
          borderRadius: BorderRadius.circular(12),
          border: Border.all(color: selected ? AppTheme.accentPrimary : AppTheme.borderColor, width: selected ? 2 : 1),
        ),
        child: Column(
          children: [
            Icon(icon, color: selected ? AppTheme.accentPrimary : AppTheme.textSecondary, size: 22),
            const SizedBox(height: 6),
            Text(label, style: TextStyle(color: selected ? AppTheme.accentPrimary : AppTheme.textSecondary, fontWeight: FontWeight.w600, fontSize: 12)),
          ],
        ),
      ),
    );
  }
}
