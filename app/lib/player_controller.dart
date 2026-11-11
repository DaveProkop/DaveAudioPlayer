import 'dart:async';

import 'package:flutter_rust_bridge/flutter_rust_bridge.dart' show Int64List;
import 'package:flutter/foundation.dart';

import 'src/rust/api/library.dart';
import 'src/rust/api/player.dart';

/// Mirrors the Rust engine state for the UI. Fed by the engine event stream; the playback
/// position is extrapolated locally from (position, timestamp, rate) instead of being
/// streamed every frame.
class PlayerController extends ChangeNotifier {
  PlayerStateDto? state;
  List<QueueItemDto> queue = const [];
  final Map<int, TrackInfo> _tracks = {};

  bool scanning = false;
  String? scanFolder;
  int scanProcessed = 0;
  int scanTotal = 0;

  /// Increments when the library content changed (after a scan).
  int libraryVersion = 0;

  /// Last error or skipped-track message, for a snackbar.
  final messages = StreamController<String>.broadcast();

  StreamSubscription<AppEvent>? _events;

  Future<void> start(String dbPath) async {
    await startEngine(dbPath: dbPath);
    _events = appEvents().listen(_onEvent);
  }

  @override
  void dispose() {
    // Stop the engine first: that closes the event stream from the Rust side, so the
    // subscription does not wait for an event that never comes.
    stopEngine().whenComplete(() => _events?.cancel());
    messages.close();
    super.dispose();
  }

  bool get isPlaying => state?.status == PlayStatus.playing;

  int? get currentIndex => state?.index;

  TrackInfo? get currentTrack {
    final id = state?.trackId;
    return id == null ? null : _tracks[id];
  }

  TrackInfo? track(int trackId) => _tracks[trackId];

  /// Live position: last reported position advanced by the time since it was reported.
  int get positionMs {
    final s = state;
    if (s == null) return 0;
    final elapsed = DateTime.now().millisecondsSinceEpoch - s.atEpochMs;
    var pos = s.positionMs + (elapsed * s.rate).round();
    final duration = s.durationMs;
    if (duration != null && pos > duration) pos = duration;
    return pos < 0 ? 0 : pos;
  }

  Future<void> _onEvent(AppEvent e) async {
    switch (e.kind) {
      case AppEventKind.state:
        state = e.state;
      case AppEventKind.queue:
        queue = e.queue ?? const [];
        await _loadMissingTracks();
      case AppEventKind.trackFailed:
        final t = e.trackId == null ? null : _tracks[e.trackId!];
        messages.add('Přeskočeno: ${t?.title ?? 'skladba'} (${e.message ?? ''})');
      case AppEventKind.error:
        messages.add('Chyba: ${e.message ?? ''}');
      case AppEventKind.scanProgress:
        scanning = true;
        scanFolder = e.scanFolder;
        scanProcessed = e.scanProcessed ?? 0;
        scanTotal = e.scanTotal ?? 0;
      case AppEventKind.scanFinished:
        scanning = false;
        libraryVersion++;
        final s = e.scanSummary;
        if (s != null) {
          messages.add(s.cancelled
              ? 'Skenování zrušeno'
              : 'Knihovna: +${s.added} nových, ${s.updated} změněných, '
                  '${s.missing} chybějících, ${s.failed} chyb');
        }
    }
    notifyListeners();
  }

  Future<void> _loadMissingTracks() async {
    final missing = queue.map((q) => q.trackId).where((id) => !_tracks.containsKey(id)).toSet();
    if (missing.isEmpty) return;
    for (final t in await tracksByIds(ids: Int64List.fromList(missing.toList()))) {
      _tracks[t.id] = t;
    }
  }

  void remember(Iterable<TrackInfo> tracks) {
    for (final t in tracks) {
      _tracks[t.id] = t;
    }
  }

  /// Appends a track and starts it when nothing is playing.
  Future<void> addAndMaybePlay(TrackInfo t) async {
    remember([t]);
    final index = queue.length;
    await enqueue(trackIds: Int64List.fromList([t.id]));
    if (!isPlaying) await playIndex(index: index);
  }
}
