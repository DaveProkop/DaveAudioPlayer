package cz.daveprokop.daveaudioplayer

import android.Manifest
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.BatteryManager
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.provider.MediaStore
import com.ryanheise.audioservice.AudioServiceActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.EventChannel
import io.flutter.plugin.common.MethodChannel

/**
 * Platform glue for lib/platform.dart: app data folder, music folder and the permission to
 * read audio files (READ_MEDIA_AUDIO on Android 13+, READ_EXTERNAL_STORAGE before).
 * Extends AudioServiceActivity so the playback service and the UI share one Flutter engine.
 */
class MainActivity : AudioServiceActivity() {
    private val channelName = "cz.daveprokop.daveaudioplayer/platform"
    private val permissionRequest = 4711
    private var pendingPermission: MethodChannel.Result? = null

    private val audioPermission: String
        get() = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            Manifest.permission.READ_MEDIA_AUDIO
        } else {
            Manifest.permission.READ_EXTERNAL_STORAGE
        }

    /** Implemented in Rust (rust/crates/api/src/android.rs): gives the audio backend the JVM and context. */
    private external fun initNative(context: Context)

    override fun onCreate(savedInstanceState: Bundle?) {
        // Before super.onCreate: AudioServiceActivity starts the Flutter engine there, and
        // Dart starts the Rust audio engine right away, which needs the Android context.
        System.loadLibrary("dap_api")
        initNative(applicationContext)
        super.onCreate(savedInstanceState)
    }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        EventChannel(flutterEngine.dartExecutor.binaryMessenger, "$channelName/charging")
            .setStreamHandler(ChargingStream(applicationContext))
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, channelName).setMethodCallHandler { call, result ->
            when (call.method) {
                "filesDir" -> result.success(filesDir.absolutePath)
                "musicDir" -> result.success(
                    Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_MUSIC).absolutePath
                )
                "requestAudioPermission" -> requestAudioPermission(result)
                // Roots of all mounted storage volumes (internal, SD cards). Apps may not list
                // /storage itself, but each volume has an app folder under <root>/Android/.
                "storageRoots" -> result.success(
                    getExternalFilesDirs(null)
                        .mapNotNull { it?.absolutePath?.substringBefore("/Android/", "") }
                        .filter { it.isNotEmpty() }
                        .distinct()
                )
                "musicFolders" -> Thread {
                    val folders = try {
                        musicFolders()
                    } catch (e: Exception) {
                        emptyList<Map<String, Any>>()
                    }
                    runOnUiThread { result.success(folders) }
                }.start()
                else -> result.notImplemented()
            }
        }
    }

    /**
     * Folders that hold music, found through MediaStore: each music file counts towards the
     * top-level folder of its storage volume (".../0/Music/Artist/Album" → ".../0/Music").
     * Largest first.
     */
    private fun musicFolders(): List<Map<String, Any>> {
        val counts = HashMap<String, Int>()
        @Suppress("DEPRECATION") // DATA still holds the file path, which is what the scanner uses.
        val projection = arrayOf(MediaStore.Audio.Media.DATA)
        contentResolver.query(
            MediaStore.Audio.Media.EXTERNAL_CONTENT_URI,
            projection,
            "${MediaStore.Audio.Media.IS_MUSIC} != 0",
            null,
            null,
        )?.use { cursor ->
            val column = cursor.getColumnIndexOrThrow(MediaStore.Audio.Media.DATA)
            while (cursor.moveToNext()) {
                val path = cursor.getString(column) ?: continue
                val folder = topFolder(path) ?: continue
                counts[folder] = (counts[folder] ?: 0) + 1
            }
        }
        return counts.entries
            .sortedByDescending { it.value }
            .map { mapOf("path" to it.key, "count" to it.value) }
    }

    /** "/storage/emulated/0/Music/a/b.mp3" → "/storage/emulated/0/Music"; null outside /storage. */
    private fun topFolder(file: String): String? {
        val parts = file.split('/').filter { it.isNotEmpty() }
        if (parts.size < 3 || parts[0] != "storage") return null
        // Volume root: /storage/emulated/<user> or /storage/<sd-card-id>.
        val rootParts = if (parts[1] == "emulated") 3 else 2
        if (parts.size <= rootParts) return null
        // A file directly in the volume root counts for the root itself.
        val take = if (parts.size == rootParts + 1) rootParts else rootParts + 1
        return "/" + parts.take(take).joinToString("/")
    }

    /** Streams whether the phone is plugged in (the app analyzes the library then). */
    private class ChargingStream(private val context: Context) : EventChannel.StreamHandler {
        private var receiver: BroadcastReceiver? = null

        override fun onListen(arguments: Any?, sink: EventChannel.EventSink) {
            val r = object : BroadcastReceiver() {
                override fun onReceive(c: Context, intent: Intent) {
                    sink.success(intent.getIntExtra(BatteryManager.EXTRA_PLUGGED, 0) != 0)
                }
            }
            receiver = r
            // A system broadcast, so no export flag is needed; it is sticky, so the current
            // state arrives right away.
            context.registerReceiver(r, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        }

        override fun onCancel(arguments: Any?) {
            receiver?.let { context.unregisterReceiver(it) }
            receiver = null
        }
    }

    private fun requestAudioPermission(result: MethodChannel.Result) {
        if (checkSelfPermission(audioPermission) == PackageManager.PERMISSION_GRANTED) {
            result.success(true)
            return
        }
        pendingPermission?.success(false)
        pendingPermission = result
        // Ask for the playback notification at the same time (Android 13+); without it the
        // service still plays, the notification is just hidden.
        val permissions = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            arrayOf(audioPermission, Manifest.permission.POST_NOTIFICATIONS)
        } else {
            arrayOf(audioPermission)
        }
        requestPermissions(permissions, permissionRequest)
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == permissionRequest) {
            val index = permissions.indexOf(audioPermission)
            val granted = index >= 0 && grantResults.getOrNull(index) == PackageManager.PERMISSION_GRANTED
            pendingPermission?.success(granted)
            pendingPermission = null
        }
    }
}
