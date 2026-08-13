# Plenum Mobile — UI/UX Improvement Notes

Audit of the Flutter app (`mobile/lib`) against the states the Rust engine actually emits
(`src/app/types.rs` → `PlenumEvent` / `TransferEvent` / `DiscoveryEvent` / `ConnectionState`)
and against the desktop app for feature parity. Items are grouped per screen, with
cross-cutting and backend-contract issues at the end. Priority tags: **[P0]** broken or
misleading today, **[P1]** high-value improvement, **[P2]** polish.

---

## 1. Global / App shell (`main.dart`, `AndroidManifest.xml`, theme)

- **[P0] App name & branding.** The Android launcher label is `mobile`
  (`AndroidManifest.xml` → `android:label="mobile"`); change it to `Plenum`. In-app,
  each screen's AppBar shows only "Send"/"Receive"/"Settings" — add the Plenum wordmark/logo
  on the top-left of the AppBar (desktop's `Layout.tsx` does exactly this with
  `nav-logo` + `nav-title`), with the screen title beside/below it. Gives the app an
  identity and matches desktop.
- **[P0] Missing manifest permission.** `main.dart` requests
  `Permission.nearbyWifiDevices` at startup, but `NEARBY_WIFI_DEVICES` is not declared in
  the manifest, so the request silently no-ops on Android 13+. Also consider
  `CHANGE_WIFI_MULTICAST_STATE` + acquiring a multicast lock while listening — many Android
  devices drop UDP broadcast packets without it, which makes LAN discovery look "flaky" to
  the user when it's actually a platform quirk.
- **[P1] Don't ask for permissions at app launch.** The nearby-wifi prompt fires before the
  user has seen a single screen. Request it contextually — when discovery/announce actually
  starts — with a one-line rationale ("Plenum needs this to find devices on your Wi-Fi").
  Storage is already done contextually (`ReceiveStorage.ensurePermission`); make Wi-Fi match.
- **[P0] Tab switches destroy in-flight transfer UI.** `_MainScreenState` swaps
  `_screens[_selectedIndex]` directly, so switching tabs disposes `SendScreen`/`ReceiveScreen`
  state while the Rust stream keeps running: progress is lost, and stream callbacks call
  `setState` on a disposed state. Use an `IndexedStack` so all three screens stay alive, and
  add `mounted` guards in every stream callback (send screen's local-send listener has none).
- **[P1] Keep the device awake during transfers.** `CorePermissions::mobile_defaults()` has
  `background_transfer: false`, so if the screen sleeps mid-transfer the transfer dies.
  Short term: hold a wakelock (e.g. `wakelock_plus`) while a transfer/listen is active and
  show "Keep the app open during transfer". Long term: Android foreground service with a
  progress notification.
- **[P2] Dark theme only.** Desktop settings offer theme + accent color; mobile hardcodes
  `AppTheme.darkTheme`. Add light/dark/system toggle in Settings for parity.

---

## 2. Send screen (`screens/send_screen.dart`)

### Device list / local mode

- **[P0] Peers show as "unknown" — fetch device name via Android API.** The engine's beacon
  hostname comes from `COMPUTERNAME`/`HOSTNAME` env vars (`src/discovery/beacon.rs:364`),
  which don't exist on Android, so every phone announces as `unknown` and the sender sees
  "Unknown Device". Plumb a caller-supplied device name through the FFI:
  - Flutter: get the user-visible name via `Settings.Global.DEVICE_NAME` (MethodChannel) with
    `Build.MODEL` (`device_info_plus`) as fallback.
  - FFI/engine: add `device_name: Option<String>` to `startReceive`/`ReceiveRequest` and use
    it in `Beacon::broadcast` instead of the env-var hostname.
  - Settings: let the user override the advertised name ("Device name" field).
- **[P1] Discovery lifecycle is opaque.** Discovery auto-runs for a fixed 10 s on screen
  open, then silently stops; `PeerNotFound` is never handled. Show a "Searching…" row/shimmer
  while `_isDiscovering`, and when it ends with no peers show the empty state *with* a
  prominent "Search again" button (the refresh icon in the AppBar is easy to miss). Consider
  re-running discovery automatically while the tab is visible (periodic rescan) so the list
  feels live. Show a device-type icon (the list hardcodes `Icons.computer` even for phones —
  the beacon could carry a platform hint).
- **[P1] Peer list details.** Show when each peer was last seen and dedupe by address as well
  as token. Manual IP entries persist forever with token `'manual'` and duplicate on re-add;
  validate the IP format and allow removing/long-press-deleting entries.
- **[P2] Whole-row tap to send.** Only the small trailing send icon is tappable; make the
  entire `ListTile` trigger the send flow, and when no file is selected explain why the
  action is disabled (snackbar "Select a file first") instead of a silently disabled icon.

### File selection

- **[P1] Show file metadata & allow clearing.** After picking, only the file name shows. Add
  file size (and icon by type), an "x" to clear the selection, and re-tap to change file.
  Sending a 2 GB file with no size warning is a surprise on the receiving end.
- **[P1] Multi-file support.** `FilePicker.pickFiles()` is single-file. Even if the engine is
  one-file-per-session today, the UI can queue files and run sessions sequentially.
- **[P2] Share-sheet intent.** Register as a share target (`receive_sharing_intent`) so users
  can send from Gallery/Files directly — the most common entry point for transfer apps.

### PIN dialog

- **[P0] The PIN dialog is misleading (and the PIN isn't enforced — see §6).** The dialog
  says "If the receiver requires a PIN…", but the receive screen labels its token
  "PIN Required". In reality the token only *filters discovery* and is never verified on
  connect. Until backend enforcement lands (§6), reword both sides so they agree; once a
  "require PIN" toggle exists, only show the PIN prompt when the peer's beacon indicates a
  PIN is required, instead of interrupting every single send with an optional dialog.
- **[P2] PIN input ergonomics.** Use `keyboardType: number` (if tokens are numeric),
  autofocus, and submit-on-enter in the PIN dialog.

### Transfer status / progress

- **[P0] No cancel.** Once `startSend`/`startSendRemote` begins there is no way to stop it —
  no UI affordance and no FFI hook (§6). Add a Cancel button on the status card for both
  modes, and a Pause/Resume pair once the engine exposes it (the protocol already has
  checkpoint/resume: `TransferEvent::Resumed`, `resumed_bytes`, `CheckpointUpdated`).
- **[P1] Progress card is bare.** Show percentage, transferred/total bytes (human-readable),
  transfer speed, and ETA — everything needed is already in `Progress` events, and
  `Completed(TransferSummary)` carries `elapsed_ms`/`total_bytes` for a final "128 MB in 14 s
  (9.1 MB/s)" summary line.
- **[P0] Local send has no error handling.** `_sendToPeer` calls `.listen(_handleTransferEvent)`
  with no `onError`, so a refused connection/engine error becomes an unhandled stream error and
  the UI just sits on the last status. Mirror the remote path's `onError`, and translate raw
  anyhow strings ("Send failed: Connection refused (os error 111)") into friendly, actionable
  messages ("Couldn't reach the device. Make sure it's still listening and on the same
  network.") with a Retry button.
- **[P1] Surface `StateChanged: Closed` and `Log(level: Warn/Error)`.** `Closed` is filtered
  out on both screens, so a dropped connection leaves a stale "Connected to device…" status
  forever. Warn/Error logs are `print`ed (TEMP DIAG) and never shown to the user.
- **[P2] Post-success state.** After "Sent successfully!" the file stays selected and the
  status card lingers. Offer "Send another" / auto-reset after a beat.

### Internet mode

- **[P1] Room code entry UX.** Add a paste button, input formatting (monospace, chunked
  like `ABC-DEF-GHI` if the 9-char code allows), and length validation before enabling
  Connect. Best: let the receiver display a QR code and the sender scan it — eliminates
  typing entirely for in-person internet transfers.
- **[P1] "Connecting to relay…" can hang 30 s with no escape.** While `_isConnectingRemote`
  the Connect button is disabled but there's no cancel and no elapsed indication. Add a
  spinner with cancel, and distinct status text for each backend state
  (`SignalingConnected` → "Contacted relay, waiting for receiver…", `NegotiatingIce` →
  "Establishing direct connection…").
- **[P2] Validation messages appear in the status card.** "Please select a file first" is
  rendered in the same slot as transfer state; use inline field errors/snackbars so state
  and validation don't share a channel.

---

## 3. Receive screen (`screens/receive_screen.dart`)

- **[P0] Receiver never shows its IP/port, but the sender's Manual-IP dialog says "Enter the
  IP address shown on the receiver's screen".** The engine binds port 0 (random) and emits the
  real port in `BroadcastStarted { port }`, which the UI drops; the sender dialog meanwhile
  defaults to 8080. Show "Your address: 192.168.1.5:38412" (local IP via `network_info_plus` +
  port from the event) under the radar so the manual flow actually works end-to-end.
- **[P0] Mode switching leaks sessions.** Switching Local → Internet → Local: the remote
  session (600 s timeout) keeps running with no cancel; going back to Internet calls
  `_setupRemoteReceiver` again, generating a *new* room code while the old session may still
  be alive. Similarly, once listening locally there is no way to stop (radar tap is disabled
  while listening). Both need a cancel/stop (§6) wired to mode switches and an explicit
  "Stop receiving" affordance.
- **[P1] "PIN Required" card is mislabeled.** The token shown is a discovery/pairing code,
  not an enforced PIN (§6). Until enforcement exists, call it what it is ("Pairing code —
  senders can use this to find you"), because today a user can reasonably believe transfers
  without the PIN are blocked when they are not.
- **[P1] Auto-accept is silent.** Any sender on the LAN can push a file and it lands directly
  in `Download/` with no prompt. Add an incoming-transfer confirmation ("*Atharva's PC* wants
  to send *report.pdf* (4.2 MB) — Accept / Decline"), with an "always accept" toggle in
  Settings for trusted setups. (Needs a small engine hook: an accept gate between `Connected`
  and `Started`.)
- **[P1] Completion is a dead end.** On completion listening stops (`_isListening = false`)
  and the status just says "Transfer complete!". Add: file name + where it was saved, an
  "Open" / "Share" action (`open_filex` / `share_plus`), and a "Receive another" button or
  auto-relisten. Currently the user must know to tap the radar again.
- **[P1] Progress parity with send screen.** Same additions: %, bytes, speed, ETA, cancel.
  Also handle `Resumed` ("Resuming from 43%…") since the engine emits it.
- **[P1] Storage permission dead-end.** If MANAGE_EXTERNAL_STORAGE is denied, the message
  "Storage permission needed to save files" is terminal. Explain *why* (files are saved to
  your public Download folder), offer a button that opens the system settings page
  (`openAppSettings()`), and re-check on return. Long-term: use MediaStore/SAF so the scary
  "All files access" grant isn't needed at all (§6).
- **[P2] Internet mode placeholder.** The static globe icon feels dead next to the animated
  radar. Reuse the radar (or a pulse animation) in internet mode while waiting; show a
  countdown or at least "Code valid while this screen is open" for the 600 s window, and a
  "New code" button after expiry instead of a raw timeout error.
- **[P2] Room code card: add a Share button** (system share sheet) next to copy — the code
  usually has to travel to another person, not the clipboard.
- **[P2] Error recovery.** `onError` sets `Error: $e` and stops listening; add Retry, and
  friendly wording per error class (port in use, relay unreachable, timeout).

---

## 4. Settings screen (`screens/settings_screen.dart`)

Currently only an About card. Everything below is additive; the desktop `SettingsPage`
(theme/color/tray/receive toggles) is the parity reference.

- **[P0] "Require PIN" toggle** — receiver-side switch: when on, senders must supply the
  pairing code before a transfer is accepted (needs engine enforcement, §6). This is the
  flagship security setting and gives the existing PIN UI real meaning.
- **[P1] Device name** — editable field, default from Android `Settings.Global.DEVICE_NAME` /
  `Build.MODEL`, persisted in `shared_preferences`, fed into the beacon announce (§2).
- **[P1] Save location** — show the current receive folder (`Download/`), and eventually let
  the user pick one (SAF directory picker). Even read-only display today would answer the #1
  post-receive question: "where did my file go?".
- **[P1] Auto-accept incoming files** toggle (see §3) — default off.
- **[P2] Theme** — light/dark/system + accent color (desktop parity).
- **[P2] Default transfer mode** — start on Local or Internet.
- **[P2] Advanced (collapsed)** — relay server URL + STUN/TURN list. Note:
  `InternetSettings.loadRelayServerUrl/saveIceServers` already exist and are *never used* —
  either wire them into an Advanced section here or delete the dead code; today
  `config.dart` hardcodes the relay and the persistence layer is orphaned.
- **[P2] About** — read version via `package_info_plus` instead of the hardcoded "0.1.0";
  add license/link to repo.
- **[P1] Transfer history** — a simple local log (file name, peer, direction, size, time,
  status) with tap-to-open for received files. `TransferSummary` already contains everything
  needed; persist it on `Completed`.

---

## 5. Backend states the UI ignores (event-contract audit)

From `PlenumEvent` in `src/app/types.rs`, currently dropped on the floor:

| Event / state | Screen behavior today | Should be |
|---|---|---|
| `StateChanged: Closed` | Explicitly filtered on both screens | "Connection closed" status; reset UI to idle |
| `Discovery: PeerNotFound` | Ignored | Empty-state message + retry CTA |
| `Discovery: SearchStarted` | Ignored | Drive the "Searching…" indicator from the event, not local flags |
| `Discovery: BroadcastStarted.port` | Dropped (only token kept) | Show receiver address (§3) |
| `Transfer: Resumed` | Ignored | "Resuming from N%" status |
| `Transfer: CheckpointUpdated` | Ignored | (fine to ignore in UI; useful for a debug view) |
| `Transfer: Started.total_bytes` / `resumed_bytes` | Ignored | Seed the progress card with size before first `Progress` |
| `Completed(TransferSummary)` fields (`elapsed_ms`, `peer`, …) | Only used as a "done" signal | Success summary + history entry |
| `Log { level: Warn/Error }` | `print()` (TEMP DIAG) | Surface Warn/Error to the user; route all levels to a real logger, remove the prints |

Also note both screens duplicate `_friendlyState` and the `_TransferMode` enum — extract a
shared `transfer_status.dart` so wording can't drift between Send and Receive.

---

## 6. Engine/FFI changes these improvements depend on

The UI can't fix these alone; listing so the mobile work can be sequenced:

1. **Cancellation handle.** `start_send/receive[_remote]` are blocking, run-to-completion
   calls; the only stop mechanism is the internal broadcast `stop_flag`. Expose a
   `cancel(session_id)` FFI (or a cancellation flag polled by the transfer loop) so the UI
   can implement Cancel, Stop-listening, and clean mode switches. Pause/resume can ride the
   existing checkpoint machinery.
2. **PIN enforcement.** `discovery_token` only filters beacon discovery
   (`discover_with_token`); `receive_file` accepts any TCP connection with no token check
   (engine.rs `TcpTransport::accept` path). Add a handshake step that verifies the pairing
   token when "require PIN" is set, and reflect "PIN required" in the beacon payload so
   senders know to prompt.
3. **Device name in announce.** Replace env-var `hostname()` with a caller-supplied name on
   `ReceiveRequest` (§2). Consider adding a platform/device-type byte to the beacon for
   correct peer icons.
4. **Accept gate for incoming transfers.** An engine callback/event between `Connected` and
   `Started` that the UI can approve/decline, enabling the incoming-file prompt (§3).
5. **Scoped storage.** The Rust engine writes with raw `std::fs` paths, forcing
   MANAGE_EXTERNAL_STORAGE. Receiving into app-private storage and moving to `Download/` via
   MediaStore on the Dart side would drop the "All files access" settings-page grant — the
   single biggest permission-UX improvement available.

---

## Suggested order of attack

1. **P0 correctness:** manifest fixes + app label; IndexedStack + `mounted` guards; `onError`
   on local send; show receiver IP:port; stop filtering `Closed`.
2. **P0/P1 engine-backed:** cancel/stop; device name plumbing; require-PIN toggle +
   enforcement; PIN wording cleanup.
3. **P1 experience:** progress %+speed+ETA; post-transfer actions (open/share/receive
   another); incoming-transfer prompt; settings screen build-out; transfer history.
4. **P2 polish:** QR pairing, share-sheet intent, theming, multi-file, share button on codes.
