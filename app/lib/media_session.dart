import 'dart:async';
import 'dart:io';

import 'package:audio_service/audio_service.dart';
import 'package:audio_session/audio_session.dart';

import 'player_controller.dart';
import 'src/rust/api/player.dart' as rust;

/// Android media session: foreground service (playback keeps going with the screen off),
/// notification, lock screen, headset and Bluetooth buttons, and audio focus.
///
/// Windows needs none of this: its media controls (SMTC) are driven by the Rust core.
class MediaSession {
  static Future<void> start(PlayerController controller) async {
    if (!Platform.isAndroid) return;
    final session = await AudioSession.instance;
    await session.configure(const AudioSessionConfiguration.music());
    await AudioService.init(
      builder: () => DapAudioHandler(controller, session),
      config: const AudioServiceConfig(
        androidNotificationChannelId: 'cz.daveprokop.daveaudioplayer.playback',
        androidNotificationChannelName: 'Přehrávání',
        androidNotificationIcon: 'drawable/ic_stat_music',
      ),
    );
  }
}

/// Bridges audio_service to the Rust engine. Commands go straight to Rust; the engine
/// state (position + timestamp + rate) is mirrored into [playbackState], which
/// extrapolates the position the same way the UI does.
class DapAudioHandler extends BaseAudioHandler with SeekHandler {
  DapAudioHandler(this._controller, this._session) {
    _controller.addListener(_sync);
    _interruptions = _session.interruptionEventStream.listen(_onInterruption);
    _noisy = _session.becomingNoisyEventStream.listen((_) => rust.pause());
    _sync();
  }

  final PlayerController _controller;
  final AudioSession _session;
  late final StreamSubscription<AudioInterruptionEvent> _interruptions;
  late final StreamSubscription<void> _noisy;

  bool _active = false;
  bool _resumeAfterInterruption = false;
  int? _shownQueueItem;
  int? _shownDuration;

  @override
  Future<void> play() => rust.play();

  @override
  Future<void> pause() => rust.pause();

  @override
  Future<void> stop() async {
    await rust.pause();
    await super.stop();
  }

  @override
  Future<void> skipToNext() => rust.next();

  @override
  Future<void> skipToPrevious() => rust.previous();

  @override
  Future<void> seek(Duration position) =>
      rust.seek(positionMs: position.inMilliseconds);

  @override
  Future<void> onTaskRemoved() => stop();

  void _onInterruption(AudioInterruptionEvent e) {
    if (e.begin) {
      switch (e.type) {
        case AudioInterruptionType.duck:
          _controller.setDucked(true);
        case AudioInterruptionType.pause:
        case AudioInterruptionType.unknown:
          _resumeAfterInterruption = _controller.isPlaying;
          rust.pause();
      }
    } else {
      switch (e.type) {
        case AudioInterruptionType.duck:
          _controller.setDucked(false);
        case AudioInterruptionType.pause:
          if (_resumeAfterInterruption) rust.play();
          _resumeAfterInterruption = false;
        case AudioInterruptionType.unknown:
          _resumeAfterInterruption = false;
      }
    }
  }

  void _sync() {
    final s = _controller.state;
    final playing = s?.status == rust.PlayStatus.playing;

    // Audio focus follows playback, whoever started it (UI, notification, headset).
    if (playing && !_active) {
      _active = true;
      _session.setActive(true);
    } else if (s?.status == rust.PlayStatus.stopped && _active) {
      _active = false;
      _session.setActive(false);
    }

    final track = _controller.currentTrack;
    final itemId = s?.queueItemId;
    final duration = s?.durationMs ?? track?.durationMs;
    // Lock screen and notification load the art from the cover cache file.
    final art = track?.coverLarge;
    final artUri = art == null ? null : Uri.file(art);
    if (itemId != _shownQueueItem ||
        duration != _shownDuration ||
        (track != null &&
            (mediaItem.value?.title != track.title ||
                mediaItem.value?.artUri != artUri))) {
      _shownQueueItem = itemId;
      _shownDuration = duration;
      mediaItem.add(
        itemId == null || track == null
            ? null
            : MediaItem(
                id: '$itemId',
                title: track.title,
                artist: track.artist.isEmpty ? null : track.artist,
                album: track.album.isEmpty ? null : track.album,
                duration: duration == null
                    ? null
                    : Duration(milliseconds: duration),
                artUri: artUri,
              ),
      );
    }

    playbackState.add(
      PlaybackState(
        controls: [
          MediaControl.skipToPrevious,
          if (playing) MediaControl.pause else MediaControl.play,
          MediaControl.skipToNext,
        ],
        systemActions: const {MediaAction.seek},
        androidCompactActionIndices: const [0, 1, 2],
        processingState: s == null || itemId == null
            ? AudioProcessingState.idle
            : AudioProcessingState.ready,
        playing: playing,
        updatePosition: Duration(milliseconds: s?.positionMs ?? 0),
        updateTime: DateTime.fromMillisecondsSinceEpoch(s?.atEpochMs ?? 0),
        speed: s?.rate ?? 1.0,
        queueIndex: s?.index,
      ),
    );
  }

  void dispose() {
    _controller.removeListener(_sync);
    _interruptions.cancel();
    _noisy.cancel();
  }
}
