import 'dart:io';

import 'package:flutter/services.dart';

/// Small platform glue without extra packages: app data folder, default music folder and
/// the Android audio-read permission (implemented in MainActivity.kt).
class AppPlatform {
  static const _channel = MethodChannel('cz.daveprokop.daveaudioplayer/platform');

  /// Calls the activity's channel. The Flutter engine is shared with the playback service
  /// (audio_service) and may run Dart before the activity has registered its handler, so
  /// a missing handler is retried for a few seconds.
  static Future<T?> _invoke<T>(String method) async {
    for (var attempt = 0; ; attempt++) {
      try {
        return await _channel.invokeMethod<T>(method);
      } on MissingPluginException {
        if (attempt >= 50) rethrow;
        await Future<void>.delayed(const Duration(milliseconds: 100));
      }
    }
  }

  /// Where the library database lives.
  static Future<String> dataDir() async {
    if (Platform.isWindows) {
      final appData = Platform.environment['APPDATA'] ?? Directory.current.path;
      return '$appData\\DaveAudioPlayer';
    }
    return await _invoke<String>('filesDir') ?? Directory.systemTemp.path;
  }

  static Future<String> defaultMusicDir() async {
    if (Platform.isWindows) {
      final home = Platform.environment['USERPROFILE'] ?? '';
      return '$home\\Music';
    }
    return await _invoke<String>('musicDir') ?? '/storage/emulated/0/Music';
  }

  /// Android: folders that contain music (via MediaStore), largest first, as
  /// (path, number of music files). Empty elsewhere.
  static Future<List<(String, int)>> musicFolders() async {
    if (!Platform.isAndroid) return const [];
    final list = (await _invoke<List<Object?>>('musicFolders') ?? const []).cast<Map<Object?, Object?>>();
    return [for (final m in list) (m['path'] as String, m['count'] as int)];
  }

  /// Android: roots of the mounted storage volumes (internal storage, SD cards).
  static Future<List<String>> storageRoots() async {
    if (!Platform.isAndroid) return const [];
    final roots = (await _invoke<List<Object?>>('storageRoots') ?? const []).cast<String>();
    return roots.isEmpty ? const ['/storage/emulated/0'] : roots;
  }

  /// Android: whether the phone is plugged in, now and on every change.
  static Stream<bool> charging() {
    if (!Platform.isAndroid) return const Stream.empty();
    return const EventChannel('cz.daveprokop.daveaudioplayer/platform/charging')
        .receiveBroadcastStream()
        .map((e) => e == true)
        .distinct();
  }

  /// Asks for READ_MEDIA_AUDIO (Android 13+) or READ_EXTERNAL_STORAGE. True when granted.
  static Future<bool> ensureAudioPermission() async {
    if (!Platform.isAndroid) return true;
    return await _invoke<bool>('requestAudioPermission') ?? false;
  }
}
