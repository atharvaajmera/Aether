import 'dart:io';
import 'package:flutter/material.dart';
import 'package:provider/provider.dart';
import 'package:open_filex/open_filex.dart';
import '../theme.dart';
import '../services/settings_service.dart';

import '../utils/formatters.dart';

class TransferHistoryScreen extends StatelessWidget {
  const TransferHistoryScreen({super.key});

  @override
  Widget build(BuildContext context) {
    final settings = context.watch<SettingsService>();
    final history = settings.transferHistory;

    return Scaffold(
      appBar: AppBar(
        title: const Text('Transfer History', style: TextStyle(color: AppTheme.textPrimary, fontSize: 18)),
      ),
      body: history.isEmpty
          ? const Center(
              child: Text('No past transfers', style: TextStyle(color: AppTheme.textSecondary)),
            )
          : ListView.builder(
              itemCount: history.length,
              itemBuilder: (context, index) {
                final item = history[index];
                final isSend = item['direction'] == 'send';
                final fileName = item['fileName'] ?? 'Unknown file';
                final size = item['size'] ?? 0;
                final peer = item['peerName'] ?? 'Unknown device';
                final path = item['path'];
                final timestamp = item['timestamp'] != null ? DateTime.parse(item['timestamp']) : DateTime.now();
                final timeStr = timestamp.toString().substring(0, 16); 
                final durationMs = item['durationMs'];
                final durationStr = durationMs is int ? formatDuration(durationMs) : '—';
                final modeLabel = formatTransferMode(item['mode']);
                final resumedBytes = item['resumedBytes'] is int ? item['resumedBytes'] as int : 0;
                final resumedStr = resumedBytes > 0 ? ' (Resumed from ${formatBytes(resumedBytes)})' : '';

                return ListTile(
                  leading: CircleAvatar(
                    backgroundColor: AppTheme.bgCardHover,
                    child: Icon(
                      isSend ? Icons.upload : Icons.download,
                      color: isSend ? AppTheme.accentPrimary : Colors.blueAccent,
                      size: 18,
                    ),
                  ),
                  title: Text(fileName, style: const TextStyle(color: AppTheme.textPrimary)),
                  subtitle: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        '${isSend ? 'To' : 'From'} $peer • ${formatBytes(size)} • $durationStr$resumedStr',
                        style: const TextStyle(color: AppTheme.textSecondary, fontSize: 12),
                      ),
                      const SizedBox(height: 2),
                      Row(
                        children: [
                          _ModeChip(label: modeLabel ?? '—'),
                          const SizedBox(width: 6),
                          Text(timeStr, style: const TextStyle(color: AppTheme.textSecondary, fontSize: 11)),
                        ],
                      ),
                    ],
                  ),
                  isThreeLine: true,
                  onTap: (!isSend && path != null)
                      ? () async {
                          if (!await File(path).exists()) {
                            if (context.mounted) {
                              ScaffoldMessenger.of(context).showSnackBar(const SnackBar(
                                content: Text('This file was moved or deleted and can no longer be opened.'),
                              ));
                            }
                            return;
                          }
                          final result = await OpenFilex.open(path);
                          if (result.type != ResultType.done && context.mounted) {
                            ScaffoldMessenger.of(context).showSnackBar(SnackBar(
                              content: Text(result.type == ResultType.noAppToOpen
                                  ? 'No app is available to open this file type'
                                  : 'Could not open the file'),
                            ));
                          }
                        }
                      : null,
                );
              },
            ),
    );
  }
}

class _ModeChip extends StatelessWidget {
  final String label;

  const _ModeChip({required this.label});

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
      decoration: BoxDecoration(
        color: AppTheme.accentPrimary.withValues(alpha: 0.12),
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: AppTheme.accentPrimary.withValues(alpha: 0.35)),
      ),
      child: Text(
        label,
        style: const TextStyle(color: AppTheme.accentPrimary, fontSize: 10, fontWeight: FontWeight.w600),
      ),
    );
  }
}
