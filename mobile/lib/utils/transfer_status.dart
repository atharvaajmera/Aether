enum TransferMode { local, internet }

enum TransferUiPhase {
  idle,
  discovering,
  listening,
  connecting,
  awaitingApproval,
  transferring,
  cancelling,
  cancelled,
  failed,
  succeeded;

  bool get isTerminal =>
      this == TransferUiPhase.succeeded ||
      this == TransferUiPhase.cancelled ||
      this == TransferUiPhase.failed;
}

String friendlyState(String state, {bool isReceive = false}) {
  switch (state) {
    case 'Discovering':
      return 'Searching...';
    case 'Listening':
      return 'Ready to receive files';
    case 'Connecting':
      return 'Connecting to device...';
    case 'SignalingConnected':
      return isReceive ? 'Ready to receive files' : 'Connecting to device...';
    case 'NegotiatingIce':
      return 'Establishing connection...';
    case 'Connected':
      return 'Connected to device...';
    case 'Closed':
      return 'Connection closed';
    default:
      return 'Connecting to device...';
  }
}

String friendlyConnectionMode(dynamic mode) {
  switch (mode) {
    case 'Relay':
      return 'relay';
    case 'Direct':
      return 'direct connection';
    default:
      return 'local network';
  }
}

const String errorRoomNotFound = 'Room not found';
const String errorReceiverUnavailable = 'Receiver unavailable';
const String errorPermissionDenied = 'Permission denied';
const String errorFileMissing = 'File no longer exists';
const String errorConnectionTimedOut = 'Connection timed out';
const String errorTransferCancelled = 'Transfer cancelled';
const String errorUnknownTransfer = 'Unknown transfer failure';

const Set<String> _transferFailureMessages = {
  errorRoomNotFound,
  errorReceiverUnavailable,
  errorPermissionDenied,
  errorFileMissing,
  errorConnectionTimedOut,
  errorUnknownTransfer,
};

// bool flag if the error is in these categories
bool isTransferFailure(String status) => _transferFailureMessages.contains(status);

// arbitary errors
String friendlyError(Object? error) {
  final text = error?.toString().toLowerCase() ?? '';
  if (text.isEmpty) return errorUnknownTransfer;

  if (text.contains('cancel')) return errorTransferCancelled;
  if (text.contains('room')) return errorRoomNotFound;
  if (text.contains('permission') ||
      text.contains('denied') ||
      text.contains('unauthorized') ||
      text.contains('forbidden')) {
    return errorPermissionDenied;
  }
  if (text.contains('enoent') ||
      text.contains('no such file') ||
      text.contains('os error 2') ||
      (text.contains('file') &&
          (text.contains('not exist') ||
              text.contains('no longer') ||
              text.contains('not found')))) {
    return errorFileMissing;
  }
  if (text.contains('timed out') ||
      text.contains('timeout') ||
      text.contains('deadline')) {
    return errorConnectionTimedOut;
  }
  if (text.contains('refused') ||
      text.contains('unreachable') ||
      text.contains('no peers') ||
      text.contains('not found') ||
      text.contains('closed') ||
      text.contains('reset') ||
      text.contains('disconnect') ||
      text.contains('offline') ||
      text.contains('unavailable')) {
    return errorReceiverUnavailable;
  }
  return errorUnknownTransfer;
}

String friendlyLogMessage(Object? level, Object? message) {
  if (level == 'Error') return 'Something went wrong during the transfer';
  final text = message?.toString();
  return (text == null || text.isEmpty) ? 'Something went wrong' : text;
}
