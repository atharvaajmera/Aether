String formatBytes(int bytes) {
  if (bytes < 1024) return '$bytes B';
  if (bytes < 1024 * 1024) return '${(bytes / 1024).toStringAsFixed(1)} KB';
  if (bytes < 1024 * 1024 * 1024) return '${(bytes / (1024 * 1024)).toStringAsFixed(1)} MB';
  return '${(bytes / (1024 * 1024 * 1024)).toStringAsFixed(2)} GB';
}

String formatDuration(int ms) {
  final totalSeconds = ms / 1000;
  if (totalSeconds < 60) return '${totalSeconds.toStringAsFixed(1)} s';
  final minutes = totalSeconds ~/ 60;
  final seconds = (totalSeconds % 60).round();
  return '$minutes m ${seconds.toString().padLeft(2, '0')} s';
}

String? formatTransferMode(dynamic mode) {
  switch (mode) {
    case 'Lan':
      return 'LAN';
    case 'Direct':
      return 'Direct';
    case 'Relay':
      return 'Relay';
    default:
      return null;
  }
}
