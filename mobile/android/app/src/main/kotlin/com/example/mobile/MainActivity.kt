package com.example.mobile

import android.content.ContentValues
import android.content.Context
import android.media.MediaScannerConnection
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Environment
import android.os.PowerManager
import android.provider.MediaStore
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import java.io.File

class MainActivity : FlutterActivity() {
    private val channelName = "plenum/media"

    // Wi-Fi + CPU locks held only for the duration of a transfer. Without the
    // high-perf Wi-Fi lock the radio parks in power-save between the sparse
    // cumulative ACKs of a large streaming transfer, silently killing the TCP
    // socket; the partial wake lock keeps the CPU alive if the screen sleeps.
    private var wifiLock: WifiManager.WifiLock? = null
    private var wakeLock: PowerManager.WakeLock? = null

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, channelName)
            .setMethodCallHandler { call, result ->
                when (call.method) {
                    "acquireTransferLock" -> {
                        try {
                            acquireTransferLock()
                            result.success(true)
                        } catch (e: Exception) {
                            result.error("LOCK_FAILED", e.message, null)
                        }
                    }
                    "releaseTransferLock" -> {
                        releaseTransferLock()
                        result.success(true)
                    }
                    "scanFile" -> {
                        val path = call.argument<String>("path")
                        if (path.isNullOrEmpty()) {
                            result.error("NO_PATH", "path argument missing", null)
                        } else {
                            // Register the raw-written file with MediaStore so it
                            // becomes visible in Files / Downloads / Gallery. Files
                            // created via a plain filesystem path are not indexed
                            // automatically on Android 10+.
                            MediaScannerConnection.scanFile(
                                applicationContext,
                                arrayOf(path),
                                null,
                                null
                            )
                            result.success(true)
                        }
                    }
                    "saveToDownloads" -> {
                        // Copies a file the Rust engine wrote into app-private
                        // storage out to the user's public Downloads, using
                        // MediaStore so no storage permission is needed on
                        // Android 10+ (scoped storage).
                        val path = call.argument<String>("path")
                        if (path.isNullOrEmpty()) {
                            result.error("NO_PATH", "path argument missing", null)
                            return@setMethodCallHandler
                        }
                        val source = File(path)
                        if (!source.exists()) {
                            result.error("NOT_FOUND", "source file does not exist: $path", null)
                            return@setMethodCallHandler
                        }
                        Thread {
                            try {
                                val saved = exportToDownloads(source)
                                runOnUiThread { result.success(saved) }
                            } catch (e: Exception) {
                                runOnUiThread {
                                    result.error("SAVE_FAILED", e.message, null)
                                }
                            }
                        }.start()
                    }
                    else -> result.notImplemented()
                }
            }
    }

    private fun acquireTransferLock() {
        if (wifiLock == null) {
            val wifiManager =
                applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
            val mode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                WifiManager.WIFI_MODE_FULL_LOW_LATENCY
            } else {
                @Suppress("DEPRECATION")
                WifiManager.WIFI_MODE_FULL_HIGH_PERF
            }
            wifiLock = wifiManager.createWifiLock(mode, "plenum:transfer")
        }
        wifiLock?.let { if (!it.isHeld) it.acquire() }

        if (wakeLock == null) {
            val powerManager =
                applicationContext.getSystemService(Context.POWER_SERVICE) as PowerManager
            wakeLock = powerManager.newWakeLock(
                PowerManager.PARTIAL_WAKE_LOCK,
                "plenum:transfer"
            )
        }
        // The Rust operation owns the transfer lifetime. A timed wake lock can
        // expire during a slow multi-gigabyte transfer and strand the socket.
        wakeLock?.let { if (!it.isHeld) it.acquire() }
    }

    /// Releases both locks if held. Idempotent.
    private fun releaseTransferLock() {
        wifiLock?.let { if (it.isHeld) it.release() }
        wakeLock?.let { if (it.isHeld) it.release() }
    }

    override fun onDestroy() {
        // Dart releases locks when the Rust stream completes. Releasing here
        // would interrupt a transfer during activity recreation.
        super.onDestroy()
    }

    /// Copies `source` into public Downloads and returns a display string of
    /// where it landed ("Download/<name>"). Duplicate names are handled by
    /// MediaStore (API 29+, appends " (1)" etc.) or manually below (API < 29).
    private fun exportToDownloads(source: File): String {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            val values = ContentValues().apply {
                put(MediaStore.Downloads.DISPLAY_NAME, source.name)
                put(MediaStore.Downloads.IS_PENDING, 1)
            }
            val resolver = applicationContext.contentResolver
            val uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
                ?: throw IllegalStateException("MediaStore insert failed")
            resolver.openOutputStream(uri).use { out ->
                if (out == null) throw IllegalStateException("cannot open MediaStore stream")
                source.inputStream().use { it.copyTo(out) }
            }
            values.clear()
            values.put(MediaStore.Downloads.IS_PENDING, 0)
            resolver.update(uri, values, null, null)
            // Resolve the final display name (MediaStore may have renamed to
            // avoid a collision).
            var name = source.name
            resolver.query(uri, arrayOf(MediaStore.Downloads.DISPLAY_NAME), null, null, null)
                ?.use { cursor ->
                    if (cursor.moveToFirst()) name = cursor.getString(0)
                }
            return "Download/$name"
        }

        // Android 9 and below: direct copy into the public folder (covered by
        // the legacy WRITE_EXTERNAL_STORAGE permission), then media-scan it.
        @Suppress("DEPRECATION")
        val downloads = Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS)
        downloads.mkdirs()
        var target = File(downloads, source.name)
        var counter = 1
        while (target.exists()) {
            val dot = source.name.lastIndexOf('.')
            target = if (dot > 0) {
                File(downloads, "${source.name.substring(0, dot)} ($counter)${source.name.substring(dot)}")
            } else {
                File(downloads, "${source.name} ($counter)")
            }
            counter++
        }
        source.copyTo(target)
        MediaScannerConnection.scanFile(applicationContext, arrayOf(target.path), null, null)
        return "Download/${target.name}"
    }
}
