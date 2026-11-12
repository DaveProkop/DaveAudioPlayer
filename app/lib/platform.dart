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

  /// Android: storage volumes to browse (internal storage, SD cards).
  static List<String> storageRoots() {
    if (!Platform.isAndroid) return const [];
    final roots = <String>['/storage/emulated/0'];
    try {
      for (final e in Directory('/storage').listSync()) {
        final name = e.path.split('/').last;
        if (e is Directory && name != 'emulated' && name != 'self') roots.add(e.path);
      }
    } catch (_) {}
    return roots;
  }

  /// Asks for READ_MEDIA_AUDIO (Android 13+) or READ_EXTERNAL_STORAGE. True when granted.
  static Future<bool> ensureAudioPermission() async {
    if (!Platform.isAndroid) return true;
    return await _channel.invokeMethod<bool>('requestAudioPermission') ?? false;
  }
}
