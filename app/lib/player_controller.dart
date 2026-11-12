import 'dart:async';

import 'package:flutter_rust_bridge/flutter_rust_bridge.dart' show Int64List;
import 'package:flutter/foundation.dart';

import 'src/rust/api/library.dart';
import 'src/rust/api/player.dart';
import 'src/rust/api/player.dart' as rust show setVolume;

/// Mirrors the Rust engine state for the UI. Fed by the engine event stream; the playback
/// position is extrapolated locally from (position, timestamp, rate) instead of being
/// streamed every frame.
class PlayerController extends ChangeNotifier {
  PlayerStateDto? state;

  /// Playback preferences as stored by the engine (sent on start and on every change).
  PlayerSettingsDto? settings;
  List<QueueItemDto> queue = const [];
  final Map<int, TrackInfo> _tracks = {};
  final Map<int, TrackDetails?> _details = {};

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

  /// User volume 0–1 as stored by the engine.
  double get volume => settings?.volume ?? 1.0;

  Future<void> setVolume(double v) => rust.setVolume(volume: v);

  /// Another app speaks (navigation, notification): the engine lowers the output.
  Future<void> setDucked(bool ducked) => duck(enabled: ducked);

  int? get currentIndex => state?.index;

  TrackInfo? get currentTrack {
    final id = state?.trackId;
    return id == null ? null : _tracks[id];
  }

  TrackInfo? track(int trackId) => _tracks[trackId];

  /// The queue entry after the current one, if loaded.
  TrackInfo? get nextTrack {
    final i = currentIndex;
    if (i == null || i + 1 >= queue.length) return null;
    return _tracks[queue[i + 1].trackId];
  }

  /// Whether `next` follows `prev` on the same album (the engine plays it gapless).
  static bool continuesAlbum(TrackInfo? prev, TrackInfo? next) {
    if (prev == null || next == null || prev.albumId == null) return false;
    final a = prev.trackNo, b = next.trackNo;
    return prev.albumId == next.albumId && a != null && b != null && b == a + 1;
  }

  /// Technical details of a track (format, loudness), loaded on demand.
  TrackDetails? details(int trackId) {
    if (!_details.containsKey(trackId)) {
      _details[trackId] = null;
      trackDetails(trackId: trackId).then((d) {
        _details[trackId] = d;
        notifyListeners();
      }, onError: (_) {});
    }
    return _details[trackId];
  }

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
      case AppEventKind.settings:
        settings = e.settings;
      case AppEventKind.queue:
        queue = e.queue ?? const [];
        await _loadMissingTracks();
      case AppEventKind.trackFailed:
        final t = e.trackId == null ? null : _tracks[e.trackId!];
        messages.add('Přeskočeno: ${t?.title ?? 'skladba'} (${e.message ?? ''})');
      case AppEventKind.error:
        messages.add('Chyba: ${e.message ?? ''}');
      case AppEventKind.libraryChanged:
        libraryVersion++;
      case AppEventKind.scanProgress:
        scanning = true;
        scanFolder = e.scanFolder;
        scanProcessed = e.scanProcessed ?? 0;
        scanTotal = e.scanTotal ?? 0;
      case AppEventKind.scanFinished:
        scanning = false;
        libraryVersion++;
        final s = e.scanSummary;
        // Scans on start and on folder changes stay silent unless something changed.
        final changed = s != null && (s.cancelled || s.added + s.updated + s.moved + s.missing + s.failed > 0);
        if (s != null && changed) {
          messages.add(
            s.cancelled
                ? 'Skenování zrušeno'
                : 'Knihovna: +${s.added} nových, ${s.updated} změněných, '
                      '${s.missing} chybějících, ${s.failed} chyb',
          );
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

  Int64List _ids(List<TrackInfo> tracks) => Int64List.fromList([for (final t in tracks) t.id]);

  /// Where "play next" inserts: right after the current item.
  int get _afterCurrent => currentIndex == null ? queue.length : currentIndex! + 1;

  /// Plays a track now: it goes right after the current one, the queue stays.
  Future<void> playNow(TrackInfo t) async {
    remember([t]);
    final at = _afterCurrent;
    await enqueue(trackIds: _ids([t]), at: at);
    await playIndex(index: at);
  }

  /// Replaces the queue with `tracks` and starts at `start` (album, artist, list).
  Future<void> playAll(List<TrackInfo> tracks, {int start = 0}) async {
    if (tracks.isEmpty) return;
    remember(tracks);
    await clearQueue();
    await enqueue(trackIds: _ids(tracks));
    await playIndex(index: start);
  }

  Future<void> playNext(List<TrackInfo> tracks) async {
    remember(tracks);
    await enqueue(trackIds: _ids(tracks), at: _afterCurrent);
    messages.add(tracks.length == 1 ? 'Zahraje se jako další' : '${tracks.length} skladeb zahraje jako další');
  }

  Future<void> addToQueue(List<TrackInfo> tracks) async {
    remember(tracks);
    await enqueue(trackIds: _ids(tracks));
    messages.add(tracks.length == 1 ? 'Přidáno do fronty' : '${tracks.length} skladeb přidáno do fronty');
  }
}
