package cz.daveprokop.daveaudioplayer

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import android.os.Environment
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

/**
 * Platform glue for lib/platform.dart: app data folder, music folder and the permission to
 * read audio files (READ_MEDIA_AUDIO on Android 13+, READ_EXTERNAL_STORAGE before).
 */
class MainActivity : FlutterActivity() {
    private val channelName = "cz.daveprokop.daveaudioplayer/platform"
    private val permissionRequest = 4711
    private var pendingPermission: MethodChannel.Result? = null

    private val audioPermission: String
        get() = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            Manifest.permission.READ_MEDIA_AUDIO
        } else {
            Manifest.permission.READ_EXTERNAL_STORAGE
        }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, channelName).setMethodCallHandler { call, result ->
            when (call.method) {
                "filesDir" -> result.success(filesDir.absolutePath)
                "musicDir" -> result.success(
                    Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_MUSIC).absolutePath
                )
                "requestAudioPermission" -> requestAudioPermission(result)
                else -> result.notImplemented()
            }
        }
    }

    private fun requestAudioPermission(result: MethodChannel.Result) {
        if (checkSelfPermission(audioPermission) == PackageManager.PERMISSION_GRANTED) {
            result.success(true)
            return
        }
        pendingPermission?.success(false)
        pendingPermission = result
        requestPermissions(arrayOf(audioPermission), permissionRequest)
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == permissionRequest) {
            val granted = grantResults.isNotEmpty() && grantResults[0] == PackageManager.PERMISSION_GRANTED
            pendingPermission?.success(granted)
            pendingPermission = null
        }
    }
}
