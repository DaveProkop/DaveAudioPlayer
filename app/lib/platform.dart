import 'dart:io';

import 'package:flutter/services.dart';

/// Small platform glue without extra packages: app data folder, default music folder and
/// the Android audio-read permission (implemented in MainActivity.kt).
class AppPlatform {
  static const _channel = MethodChannel('cz.daveprokop.daveaudioplayer/platform');

  /// Where the library database lives.
  static Future<String> dataDir() async {
    if (Platform.isWindows) {
      final appData = Platform.environment['APPDATA'] ?? Directory.current.path;
      return '$appData\\DaveAudioPlayer';
    }
    return await _channel.invokeMethod<String>('filesDir') ?? Directory.systemTemp.path;
  }

  static Future<String> defaultMusicDir() async {
    if (Platform.isWindows) {
      final home = Platform.environment['USERPROFILE'] ?? '';
      return '$home\\Music';
    }
    return await _channel.invokeMethod<String>('musicDir') ?? '/storage/emulated/0/Music';
  }

  /// Android: folders that contain music (via MediaStore), largest first, as
  /// (path, number of music files). Empty elsewhere.
  static Future<List<(String, int)>> musicFolders() async {
    if (!Platform.isAndroid) return const [];
    final list = await _channel.invokeListMethod<Map<Object?, Object?>>('musicFolders') ?? const [];
    return [for (final m in list) (m['path'] as String, m['count'] as int)];
  }

  /// Android: roots of the mounted storage volumes (internal storage, SD cards).
  static Future<List<String>> storageRoots() async {
    if (!Platform.isAndroid) return const [];
    final roots = await _channel.invokeListMethod<String>('storageRoots') ?? const [];
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
    return await _channel.invokeMethod<bool>('requestAudioPermission') ?? false;
  }
}
