import 'dart:async';
import 'dart:io';

import 'platform.dart';
import 'src/rust/api/player.dart' as rust;
import 'ui/theme.dart';

/// Android: the engine analyzes only queued tracks, except while the phone charges (and the
/// user allows it), when it works through the whole library. Desktop analyzes everything
/// anyway, so this does nothing there.
class BackgroundAnalysis {
  BackgroundAnalysis(this._prefs);

  final UiPrefs _prefs;
  StreamSubscription<bool>? _sub;
  bool _charging = false;
  bool? _sent;

  void start() {
    if (!Platform.isAndroid) return;
    _sub = AppPlatform.charging().listen((c) {
      _charging = c;
      _apply();
    });
    _prefs.addListener(_apply);
  }

  void _apply() {
    final on = _charging && _prefs.analyzeOnCharge;
    if (on == _sent) return;
    _sent = on;
    rust.setBackgroundAnalysis(enabled: on);
  }

  void dispose() {
    _sub?.cancel();
    _prefs.removeListener(_apply);
  }
}
