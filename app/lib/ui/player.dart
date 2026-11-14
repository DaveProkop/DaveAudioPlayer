import 'dart:async';

import 'package:flutter/material.dart';

import '../player_controller.dart';
import '../src/rust/api/library.dart';
import '../src/rust/api/metadata.dart';
import '../src/rust/api/stats.dart' show setRating;
import '../src/rust/api/player.dart' as rust;
import 'theme.dart';
import 'widgets.dart';

/// Rebuilds a few times a second while playing, so the extrapolated position moves.
class PositionTicker extends StatefulWidget {
  const PositionTicker({super.key, required this.player, required this.builder});

  final PlayerController player;
  final WidgetBuilder builder;

  @override
  State<PositionTicker> createState() => _PositionTickerState();
}

class _PositionTickerState extends State<PositionTicker> {
  Timer? _timer;

  @override
  void initState() {
    super.initState();
    _timer = Timer.periodic(const Duration(milliseconds: 250), (_) {
      if (mounted && widget.player.isPlaying) setState(() {});
    });
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => widget.builder(context);
}

/// Short name of a mix mode.
String mixModeName(rust.MixModeDto mode) => switch (mode) {
  rust.MixModeDto.album => 'Album',
  rust.MixModeDto.radio => 'Rádio',
  rust.MixModeDto.party => 'Párty',
  rust.MixModeDto.chill => 'Chill',
};

/// What a mix mode does, in a sentence or two.
String mixModeDescription(rust.MixModeDto mode) => switch (mode) {
  rust.MixModeDto.album => 'Skladby hrají, jak byly nahrané: bez mezery, nic se nemíchá.',
  rust.MixModeDto.radio => 'Prolnutí 6 s, ticho na začátku a konci skladeb se přeskočí.',
  rust.MixModeDto.party =>
    'Chytré přechody: beatmatch při podobném tempu, střih na dobu mezi energickými skladbami, '
        'jinak filtr nebo echo. Řazení fronty stupňuje energii.',
  rust.MixModeDto.chill =>
    'Dlouhá prolnutí 10 s, beatmatch a přechody filtrem, žádné střihy ani echo. '
        'Řazení fronty energii postupně zklidňuje.',
};

/// Crossfade length that applies at the end of the current track (0 = none).
int fadeMsAtEnd(PlayerController p) {
  final xf = p.settings?.crossfadeMs ?? 0;
  if (xf == 0 || p.nextTrack == null) return 0;
  if (PlayerController.continuesAlbum(p.currentTrack, p.nextTrack)) return 0;
  return xf;
}

/// Time slider with the crossfade zone marked at its end.
class SeekBar extends StatefulWidget {
  const SeekBar({super.key, required this.player, this.compact = false});

  final PlayerController player;
  final bool compact;

  @override
  State<SeekBar> createState() => _SeekBarState();
}

class _SeekBarState extends State<SeekBar> {
  double? _drag;

  /// After a seek: the target, shown until the engine reports a position near it (or a
  /// second passes), so the thumb does not jump back meanwhile.
  double? _target;
  DateTime _targetSince = DateTime.now();

  @override
  Widget build(BuildContext context) {
    final p = widget.player;
    final scheme = Theme.of(context).colorScheme;
    return PositionTicker(
      player: p,
      builder: (context) {
        final duration = p.state?.durationMs ?? 0;
        final target = _target;
        if (target != null &&
            ((p.positionMs - target).abs() < 1500 ||
                DateTime.now().difference(_targetSince) > const Duration(seconds: 1))) {
          _target = null;
        }
        final pos = _drag ?? _target ?? p.positionMs.toDouble();
        final fade = duration > 0 ? fadeMsAtEnd(p) / duration : 0.0;
        final time = TextStyle(
          fontSize: 12,
          color: scheme.onSurfaceVariant,
          fontFeatures: const [FontFeature.tabularFigures()],
        );
        final slider = LayoutBuilder(
          builder: (context, c) => Stack(
            alignment: Alignment.center,
            children: [
              if (fade > 0)
                Positioned(
                  right: 14,
                  width: (c.maxWidth - 28) * fade.clamp(0.0, 1.0),
                  height: 10,
                  child: Tooltip(
                    message: 'Prolnutí do další skladby',
                    child: DecoratedBox(
                      decoration: BoxDecoration(
                        borderRadius: BorderRadius.circular(3),
                        color: scheme.primary.withValues(alpha: .25),
                      ),
                    ),
                  ),
                ),
              Slider(
                value: duration > 0 ? pos.clamp(0, duration.toDouble()) : 0,
                max: duration > 0 ? duration.toDouble() : 1,
                onChanged: duration > 0 ? (v) => setState(() => _drag = v) : null,
                onChangeEnd: (v) {
                  setState(() {
                    _drag = null;
                    _target = v;
                    _targetSince = DateTime.now();
                  });
                  rust.seek(positionMs: v.round());
                },
              ),
            ],
          ),
        );
        if (widget.compact) {
          return Column(
            children: [
              slider,
              Padding(
                padding: const EdgeInsets.symmetric(horizontal: 14),
                child: Row(
                  children: [
                    Text(formatMs(pos.round()), style: time),
                    const Spacer(),
                    Text(formatMs(duration > 0 ? duration : null), style: time),
                  ],
                ),
              ),
            ],
          );
        }
        return Row(
          children: [
            SizedBox(
              width: 40,
              child: Text(formatMs(pos.round()), textAlign: TextAlign.right, style: time),
            ),
            Expanded(child: slider),
            SizedBox(width: 40, child: Text(formatMs(duration > 0 ? duration : null), style: time)),
          ],
        );
      },
    );
  }
}

/// Previous / play-pause / next.
class TransportButtons extends StatelessWidget {
  const TransportButtons({super.key, required this.player, this.large = false});

  final PlayerController player;
  final bool large;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final playing = player.isPlaying;
    final hasQueue = player.queue.isNotEmpty;
    final size = large ? 64.0 : 40.0;
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        IconButton(
          tooltip: 'Předchozí',
          iconSize: large ? 34 : 24,
          onPressed: hasQueue ? () => rust.previous() : null,
          icon: const Icon(Icons.skip_previous_rounded),
        ),
        SizedBox(width: large ? 20 : 8),
        SizedBox(
          width: size,
          height: size,
          child: IconButton.filled(
            tooltip: playing ? 'Pozastavit' : 'Přehrát',
            style: IconButton.styleFrom(backgroundColor: scheme.onSurface, foregroundColor: scheme.surface),
            iconSize: large ? 34 : 24,
            onPressed: hasQueue ? () => rust.togglePlayPause() : null,
            icon: Icon(playing ? Icons.pause_rounded : Icons.play_arrow_rounded),
          ),
        ),
        SizedBox(width: large ? 20 : 8),
        IconButton(
          tooltip: 'Další',
          iconSize: large ? 34 : 24,
          onPressed: hasQueue ? () => rust.next() : null,
          icon: const Icon(Icons.skip_next_rounded),
        ),
      ],
    );
  }
}

/// Small toggle pill (R128, crossfade, EQ).
class StatusPill extends StatelessWidget {
  const StatusPill({super.key, required this.label, required this.on, required this.tooltip, this.onTap});

  final String label;
  final bool on;
  final String tooltip;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Tooltip(
      message: tooltip,
      child: InkWell(
        borderRadius: BorderRadius.circular(6),
        onTap: onTap,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 3),
          decoration: BoxDecoration(
            color: on ? scheme.primary.withValues(alpha: .16) : scheme.surfaceContainerHighest,
            borderRadius: BorderRadius.circular(6),
          ),
          child: Text(
            label,
            style: TextStyle(fontSize: 11, letterSpacing: .3, color: on ? scheme.primary : scheme.onSurfaceVariant),
          ),
        ),
      ),
    );
  }
}

class VolumeSlider extends StatefulWidget {
  const VolumeSlider({super.key, required this.player});

  final PlayerController player;

  @override
  State<VolumeSlider> createState() => _VolumeSliderState();
}

class _VolumeSliderState extends State<VolumeSlider> {
  double? _drag;

  @override
  Widget build(BuildContext context) {
    final v = _drag ?? widget.player.volume;
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Icon(v == 0 ? Icons.volume_off : (v < .5 ? Icons.volume_down : Icons.volume_up), size: 20),
        SizedBox(
          width: 110,
          child: Slider(
            value: v,
            onChanged: (x) {
              setState(() => _drag = x);
              widget.player.setVolume(x);
            },
            onChangeEnd: (_) => setState(() => _drag = null),
          ),
        ),
      ],
    );
  }
}

/// Desktop player bar.
class PlayerBar extends StatelessWidget {
  const PlayerBar({super.key, required this.player, required this.onOpenNowPlaying, required this.onOpenEq});

  final PlayerController player;
  final VoidCallback onOpenNowPlaying;
  final VoidCallback onOpenEq;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return ListenableBuilder(
      listenable: player,
      builder: (context, _) {
        final t = player.currentTrack;
        final s = player.settings;
        return Material(
          color: scheme.surfaceContainerHigh,
          child: Container(
            height: 84,
            decoration: BoxDecoration(
              border: Border(top: BorderSide(color: scheme.outlineVariant)),
            ),
            padding: const EdgeInsets.symmetric(horizontal: 18),
            child: Row(
              children: [
                SizedBox(
                  width: 280,
                  child: InkWell(
                    onTap: t == null ? null : onOpenNowPlaying,
                    borderRadius: BorderRadius.circular(8),
                    child: Row(
                      children: [
                        CoverImage(path: t?.coverSmall, size: 56),
                        const SizedBox(width: 12),
                        Expanded(
                          child: Column(
                            mainAxisAlignment: MainAxisAlignment.center,
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              Text(
                                t?.title ?? 'Nic nehraje',
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                                style: const TextStyle(fontWeight: FontWeight.w600),
                              ),
                              if (t != null)
                                Text(
                                  [t.artist, t.album].where((x) => x.isNotEmpty).join(' · '),
                                  maxLines: 1,
                                  overflow: TextOverflow.ellipsis,
                                  style: TextStyle(fontSize: 12.5, color: scheme.onSurfaceVariant),
                                ),
                            ],
                          ),
                        ),
                      ],
                    ),
                  ),
                ),
                const SizedBox(width: 18),
                Expanded(
                  child: Column(
                    mainAxisAlignment: MainAxisAlignment.center,
                    children: [
                      TransportButtons(player: player),
                      ConstrainedBox(
                        constraints: const BoxConstraints(maxWidth: 640),
                        child: SeekBar(player: player),
                      ),
                    ],
                  ),
                ),
                const SizedBox(width: 18),
                SizedBox(
                  width: 280,
                  child: Row(
                    mainAxisAlignment: MainAxisAlignment.end,
                    children: [
                      StatusPill(
                        label: 'R128',
                        on: s?.normalize ?? false,
                        tooltip: 'Vyrovnání hlasitosti',
                        onTap: s == null ? null : () => rust.setNormalization(enabled: !s.normalize),
                      ),
                      const SizedBox(width: 6),
                      StatusPill(
                        label: s == null ? 'Mix' : mixModeName(s.mixMode),
                        on: s != null && s.mixMode != rust.MixModeDto.album,
                        tooltip: 'Režim mixu (kliknutím další)',
                        onTap: s == null
                            ? null
                            : () => rust.setMixMode(
                                mode: rust.MixModeDto.values[(s.mixMode.index + 1) % rust.MixModeDto.values.length],
                              ),
                      ),
                      const SizedBox(width: 6),
                      StatusPill(label: 'EQ', on: s?.eqEnabled ?? false, tooltip: 'Ekvalizér', onTap: onOpenEq),
                      const SizedBox(width: 10),
                      VolumeSlider(player: player),
                    ],
                  ),
                ),
              ],
            ),
          ),
        );
      },
    );
  }
}

/// Mobile mini player above the bottom navigation.
class MiniPlayer extends StatelessWidget {
  const MiniPlayer({super.key, required this.player, required this.onOpen});

  final PlayerController player;
  final VoidCallback onOpen;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return ListenableBuilder(
      listenable: player,
      builder: (context, _) {
        final t = player.currentTrack;
        if (t == null) return const SizedBox.shrink();
        return Padding(
          padding: const EdgeInsets.fromLTRB(8, 0, 8, 6),
          child: Material(
            color: scheme.surfaceContainerHighest,
            borderRadius: BorderRadius.circular(12),
            clipBehavior: Clip.antiAlias,
            child: GestureDetector(
              onVerticalDragEnd: (d) {
                if ((d.primaryVelocity ?? 0) < -200) onOpen();
              },
              child: InkWell(
                onTap: onOpen,
                child: Stack(
                  children: [
                    Padding(
                      padding: const EdgeInsets.fromLTRB(8, 8, 4, 10),
                      child: Row(
                        children: [
                          CoverImage(path: t.coverSmall, size: 40),
                          const SizedBox(width: 10),
                          Expanded(
                            child: Column(
                              crossAxisAlignment: CrossAxisAlignment.start,
                              children: [
                                Text(
                                  t.title,
                                  maxLines: 1,
                                  overflow: TextOverflow.ellipsis,
                                  style: const TextStyle(fontWeight: FontWeight.w600, fontSize: 13.5),
                                ),
                                Text(
                                  t.artist,
                                  maxLines: 1,
                                  overflow: TextOverflow.ellipsis,
                                  style: TextStyle(fontSize: 12, color: scheme.onSurfaceVariant),
                                ),
                              ],
                            ),
                          ),
                          IconButton(
                            onPressed: () => rust.togglePlayPause(),
                            icon: Icon(player.isPlaying ? Icons.pause_rounded : Icons.play_arrow_rounded),
                          ),
                          IconButton(onPressed: () => rust.next(), icon: const Icon(Icons.skip_next_rounded)),
                        ],
                      ),
                    ),
                    Positioned(
                      left: 10,
                      right: 10,
                      bottom: 3,
                      child: PositionTicker(
                        player: player,
                        builder: (context) {
                          final d = player.state?.durationMs ?? 0;
                          return LinearProgressIndicator(
                            value: d > 0 ? (player.positionMs / d).clamp(0.0, 1.0) : 0,
                            minHeight: 2,
                            borderRadius: BorderRadius.circular(2),
                            backgroundColor: Colors.white10,
                          );
                        },
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ),
        );
      },
    );
  }
}

/// Now playing: big cover, track facts, transport, what comes next and how.
class NowPlayingView extends StatelessWidget {
  const NowPlayingView({super.key, required this.player, this.onClose, this.onOpenQueue, this.onOpenEq});

  final PlayerController player;
  final VoidCallback? onClose;
  final VoidCallback? onOpenQueue;
  final VoidCallback? onOpenEq;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return ListenableBuilder(
      listenable: player,
      builder: (context, _) {
        final t = player.currentTrack;
        if (t == null) {
          return const EmptyState(icon: Icons.music_note_outlined, text: 'Nic nehraje.\nVyber skladbu v knihovně.');
        }
        final details = player.details(t.id);
        final next = player.nextTrack;
        final s = player.settings;
        final gapless = PlayerController.continuesAlbum(t, next);
        final facts = <String>[
          if (details != null) _format(details),
          if (details?.loudnessLufs != null && (s?.normalize ?? false))
            'Hlasitost ${details!.loudnessLufs!.toStringAsFixed(1).replaceAll('.', ',').replaceAll('-', '−')} → −18 LUFS',
          if (details?.bpm != null)
            '${details!.bpm!.toStringAsFixed(1).replaceAll('.', ',')} BPM · ${details.bpmFromTag ? 'z tagu' : 'odhad'}',
          if (details?.key != null) 'Tónina ${details!.key} · ${details.keyFromTag ? 'z tagu' : 'odhad'}',
          if (details?.energy != null) 'Energie ${(details!.energy! * 10).round()}/10',
          if (s?.eqEnabled ?? false) 'EQ zapnutý',
        ];
        // `compact` (phones): everything fits on one screen, lyrics open from the bar.
        Widget info(bool compact) => Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(
              t.title,
              maxLines: compact ? 2 : null,
              overflow: compact ? TextOverflow.ellipsis : null,
              style: (compact ? theme.textTheme.headlineSmall : theme.textTheme.headlineMedium)?.copyWith(
                fontWeight: FontWeight.w700,
              ),
            ),
            const SizedBox(height: 2),
            Text(
              [
                t.artist,
                if (t.album.isNotEmpty) t.album + (details?.year != null ? ' (${details!.year})' : ''),
              ].join(' · '),
              maxLines: compact ? 1 : null,
              overflow: compact ? TextOverflow.ellipsis : null,
              style: theme.textTheme.titleMedium?.copyWith(color: scheme.onSurfaceVariant),
            ),
            SizedBox(height: compact ? 10 : 14),
            if (compact)
              SingleChildScrollView(
                scrollDirection: Axis.horizontal,
                child: Row(
                  children: [
                    for (final f in facts)
                      Padding(padding: const EdgeInsets.only(right: 8), child: _FactChip(text: f)),
                  ],
                ),
              )
            else
              Wrap(
                spacing: 8,
                runSpacing: 8,
                children: [for (final f in facts) _FactChip(text: f)],
              ),
            SizedBox(height: compact ? 2 : 8),
            _RatingStars(player: player, trackId: t.id, stars: details?.rating ?? 0),
            const SizedBox(height: 8),
            SeekBar(player: player, compact: true),
            const SizedBox(height: 8),
            Center(child: TransportButtons(player: player, large: true)),
            if (next != null) ...[
              SizedBox(height: compact ? 8 : 20),
              Container(
                padding: const EdgeInsets.all(12),
                decoration: BoxDecoration(color: scheme.surfaceContainer, borderRadius: BorderRadius.circular(10)),
                child: Row(
                  children: [
                    CoverImage(path: next.coverSmall, size: 44),
                    const SizedBox(width: 12),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text('Další', style: TextStyle(fontSize: 12, color: scheme.onSurfaceVariant)),
                          Text('${next.title} · ${next.artist}', maxLines: 1, overflow: TextOverflow.ellipsis),
                          if (t.key != null && next.key != null)
                            Text(
                              keysCompatible(t.key, next.key)
                                  ? 'Tónina ${t.key} → ${next.key} · harmonicky sedí'
                                  : 'Tónina ${t.key} → ${next.key} · nesedí',
                              style: TextStyle(
                                fontSize: 12.5,
                                color: keysCompatible(t.key, next.key) ? context.appColors.ok : scheme.onSurfaceVariant,
                              ),
                            ),
                          _TransitionLine(player: player, gapless: gapless),
                        ],
                      ),
                    ),
                    if (player.state?.transition case final tr? when tr.kind != rust.TransitionKindDto.gapless)
                      PositionTicker(
                        player: player,
                        builder: (context) {
                          // 10 s of the outgoing track, then the transition itself.
                          final from = tr.startMs - _previewLeadMs;
                          final ahead = from > player.positionMs + 1000;
                          return IconButton(
                            onPressed: ahead ? () => rust.seek(positionMs: from) : null,
                            icon: const Icon(Icons.headphones_outlined),
                            tooltip: 'Poslechnout přechod',
                          );
                        },
                      ),
                  ],
                ),
              ),
            ],
            if (!compact) ...[
              const SizedBox(height: 12),
              LyricsCard(key: ValueKey(t.id), player: player, trackId: t.id),
            ],
          ],
        );
        return Container(
          decoration: BoxDecoration(
            gradient: RadialGradient(
              center: const Alignment(-.6, -.9),
              radius: 1.3,
              colors: [scheme.primary.withValues(alpha: .22), Colors.transparent],
            ),
          ),
          child: SafeArea(
            child: LayoutBuilder(
              builder: (context, c) {
                final wide = c.maxWidth >= 760;
                final cover = ConstrainedBox(
                  constraints: BoxConstraints(maxWidth: wide ? 400 : 360),
                  child: FlexCover(path: t.coverLarge, radius: 14, shadow: true),
                );
                final body = Row(
                  children: [
                    Expanded(flex: 5, child: Center(child: cover)),
                    const SizedBox(width: 36),
                    Expanded(flex: 6, child: info(false)),
                  ],
                );
                // The close bar stays at the top; the content is centred below it.
                final bar = onClose == null
                    ? null
                    : Padding(
                        padding: const EdgeInsets.fromLTRB(12, 4, 12, 0),
                        child: Row(
                          children: [
                            IconButton(
                              onPressed: onClose,
                              icon: const Icon(Icons.keyboard_arrow_down),
                              tooltip: 'Zavřít',
                            ),
                            const Spacer(),
                            if (!wide)
                              IconButton(
                                onPressed: () => showModalBottomSheet<void>(
                                  context: context,
                                  isScrollControlled: true,
                                  showDragHandle: true,
                                  builder: (_) => SafeArea(
                                    child: Padding(
                                      padding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
                                      child: SingleChildScrollView(
                                        child: LyricsCard(key: ValueKey(t.id), player: player, trackId: t.id),
                                      ),
                                    ),
                                  ),
                                ),
                                icon: const Icon(Icons.lyrics_outlined),
                                tooltip: 'Text písně',
                              ),
                            if (onOpenEq != null)
                              IconButton(onPressed: onOpenEq, icon: const Icon(Icons.equalizer), tooltip: 'Ekvalizér'),
                            if (onOpenQueue != null)
                              IconButton(
                                onPressed: onOpenQueue,
                                icon: const Icon(Icons.queue_music),
                                tooltip: 'Fronta',
                              ),
                          ],
                        ),
                      );
                final barHeight = bar == null ? 0.0 : 52.0;
                if (!wide) {
                  // Phones: no scrolling, the cover takes what the rest leaves.
                  return Column(
                    children: [
                      ?bar,
                      Expanded(
                        child: Padding(
                          padding: EdgeInsets.fromLTRB(20, bar == null ? 16 : 0, 20, 12),
                          child: Column(
                            children: [
                              Expanded(
                                child: Center(
                                  child: AspectRatio(
                                    aspectRatio: 1,
                                    child: FlexCover(path: t.coverLarge, radius: 14, shadow: true),
                                  ),
                                ),
                              ),
                              const SizedBox(height: 14),
                              info(true),
                            ],
                          ),
                        ),
                      ),
                    ],
                  );
                }
                return Column(
                  children: [
                    ?bar,
                    Expanded(
                      child: SingleChildScrollView(
                        padding: EdgeInsets.fromLTRB(24, bar == null ? 28 : 8, 24, 16),
                        child: ConstrainedBox(
                          constraints: BoxConstraints(minHeight: c.maxHeight - barHeight - 52),
                          child: Column(mainAxisAlignment: MainAxisAlignment.center, children: [body]),
                        ),
                      ),
                    ),
                  ],
                );
              },
            ),
          ),
        );
      },
    );
  }

  /// How far before a transition its preview starts.
  static const _previewLeadMs = 10000;

  static String _format(TrackDetails d) {
    final parts = <String>[d.format.toUpperCase()];
    if (d.bitDepth != null && d.sampleRate != null) {
      parts.add('${d.bitDepth} bit / ${_khz(d.sampleRate!)} kHz');
    } else if (d.bitrateKbps != null) {
      parts.add('${d.bitrateKbps} kb/s');
    }
    return parts.join(' ');
  }

  static String _khz(int hz) {
    final k = hz / 1000;
    return (k == k.roundToDouble() ? k.toStringAsFixed(0) : k.toStringAsFixed(1)).replaceAll('.', ',');
  }
}

/// How the current track passes into the next one, and in how long. Tapping it picks the
/// transition for this pair of tracks.
class _TransitionLine extends StatelessWidget {
  const _TransitionLine({required this.player, required this.gapless});

  final PlayerController player;
  final bool gapless;

  static const _choices = <(rust.TransitionKindDto?, String)>[
    (null, 'Automaticky'),
    (rust.TransitionKindDto.beatmatch, 'Beatmatch'),
    (rust.TransitionKindDto.hardCut, 'Střih na dobu'),
    (rust.TransitionKindDto.filterSweep, 'Přechod filtrem'),
    (rust.TransitionKindDto.echoOut, 'Echo na poslední době'),
    (rust.TransitionKindDto.crossfade, 'Prolnutí'),
    (rust.TransitionKindDto.gapless, 'Bez mezery'),
  ];

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final tr = player.state?.transition;
    final s = player.settings;
    final crossfadeS = (((s?.crossfadeMs ?? 0) == 0 ? 6000 : s!.crossfadeMs) / 1000).round();
    var label = switch (tr?.kind) {
      rust.TransitionKindDto.beatmatch => 'beatmatch – srovnání do rytmu',
      rust.TransitionKindDto.filterSweep => 'přechod filtrem',
      rust.TransitionKindDto.echoOut => 'echo na poslední době',
      rust.TransitionKindDto.hardCut => 'střih na dobu',
      rust.TransitionKindDto.crossfade => 'prolnutí $crossfadeS s',
      rust.TransitionKindDto.gapless => gapless ? 'bez mezery – další skladba alba' : 'bez mezery',
      null =>
        gapless
            ? 'bez mezery – další skladba alba'
            : (s?.crossfadeMs ?? 0) > 0
            ? 'prolnutí $crossfadeS s'
            : 'bez mezery',
    };
    if (tr?.forced ?? false) label = '$label · ručně';
    final style = TextStyle(fontSize: 12.5, color: gapless ? context.appColors.gapless : scheme.primary);
    final from = player.currentTrack?.id;
    final to = player.nextTrack?.id;
    final text = tr == null
        ? Text(label, style: style)
        : PositionTicker(
            player: player,
            builder: (context) {
              final left = tr.startMs - player.positionMs;
              return Text(left > 0 ? '$label · za ${formatMs(left)}' : label, style: style);
            },
          );
    if (from == null || to == null) return text;
    final chosen = (tr?.forced ?? false) ? tr!.kind : null;
    return PopupMenuButton<int>(
      tooltip: 'Vybrat přechod',
      position: PopupMenuPosition.under,
      onSelected: (i) =>
          rust.setTransitionOverride(fromTrackId: from, toTrackId: to, kind: _choices[i].$1),
      itemBuilder: (context) => [
        for (final (i, (kind, name)) in _choices.indexed)
          CheckedPopupMenuItem(value: i, checked: kind == chosen, child: Text(name)),
      ],
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Flexible(child: text),
          Icon(Icons.arrow_drop_down, size: 18, color: style.color),
        ],
      ),
    );
  }
}

/// Lyrics of the playing track: synced ones follow the playback (the current line lit,
/// a few around it), plain ones scroll. Looked up online on first use (LRCLIB).
class LyricsCard extends StatefulWidget {
  const LyricsCard({super.key, required this.player, required this.trackId});

  final PlayerController player;
  final int trackId;

  @override
  State<LyricsCard> createState() => _LyricsCardState();
}

class _LyricsCardState extends State<LyricsCard> {
  static bool _open = true;
  late final Future<LyricsDto?> _lyrics = lyrics(trackId: widget.trackId, online: true);

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      padding: const EdgeInsets.fromLTRB(12, 4, 12, 12),
      decoration: BoxDecoration(color: scheme.surfaceContainer, borderRadius: BorderRadius.circular(10)),
      child: FutureBuilder<LyricsDto?>(
        future: _lyrics,
        builder: (context, snap) {
          final l = snap.data;
          final status = switch (snap.connectionState) {
            ConnectionState.done when snap.hasError => 'Text se nepodařilo načíst',
            ConnectionState.done when l == null || l.lines.every((x) => x.text.isEmpty) => 'Text nenalezen',
            ConnectionState.done => switch (l!.source) {
              'lrclib' => 'z LRCLIB',
              'lrc_file' => 'ze souboru .lrc',
              _ => 'z tagů souboru',
            },
            _ => 'Hledám text…',
          };
          final has = l != null && l.lines.any((x) => x.text.isNotEmpty);
          return Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Text('Text', style: TextStyle(fontSize: 12, color: scheme.onSurfaceVariant)),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(status, style: TextStyle(fontSize: 12, color: scheme.outline)),
                  ),
                  if (has)
                    IconButton(
                      visualDensity: VisualDensity.compact,
                      tooltip: _open ? 'Skrýt text' : 'Zobrazit text',
                      onPressed: () => setState(() => _open = !_open),
                      icon: Icon(_open ? Icons.expand_less : Icons.expand_more),
                    ),
                ],
              ),
              if (has && _open) l.synced ? _synced(l, scheme) : _plain(l, scheme),
            ],
          );
        },
      ),
    );
  }

  Widget _synced(LyricsDto l, ColorScheme scheme) {
    return PositionTicker(
      player: widget.player,
      builder: (context) {
        final pos = widget.player.positionMs;
        var current = -1;
        for (var i = 0; i < l.lines.length && l.lines[i].ms <= pos; i++) {
          current = i;
        }
        final from = (current - 2).clamp(0, l.lines.length);
        final to = (from + 7).clamp(0, l.lines.length);
        return Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            for (var i = from; i < to; i++)
              Padding(
                padding: const EdgeInsets.symmetric(vertical: 2),
                child: AnimatedDefaultTextStyle(
                  duration: const Duration(milliseconds: 200),
                  style: TextStyle(
                    fontSize: i == current ? 17 : 15,
                    height: 1.3,
                    fontWeight: i == current ? FontWeight.w700 : FontWeight.w400,
                    color: i == current ? scheme.primary : scheme.onSurfaceVariant,
                  ),
                  child: Text(l.lines[i].text.isEmpty ? '♪' : l.lines[i].text),
                ),
              ),
          ],
        );
      },
    );
  }

  Widget _plain(LyricsDto l, ColorScheme scheme) {
    return ConstrainedBox(
      constraints: const BoxConstraints(maxHeight: 260),
      child: SingleChildScrollView(
        child: Text(
          l.lines.map((x) => x.text).join('\n'),
          style: TextStyle(fontSize: 15, height: 1.4, color: scheme.onSurfaceVariant),
        ),
      ),
    );
  }
}

/// 0–5 stars; tapping the current rating clears it.
class _RatingStars extends StatelessWidget {
  const _RatingStars({required this.player, required this.trackId, required this.stars});

  final PlayerController player;
  final int trackId;
  final int stars;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        for (var i = 1; i <= 5; i++)
          IconButton(
            visualDensity: VisualDensity.compact,
            tooltip: i == stars ? 'Zrušit hodnocení' : 'Hodnotit $i/5',
            onPressed: () async {
              await setRating(trackId: trackId, stars: i == stars ? 0 : i);
              player.forgetDetails(trackId);
            },
            icon: Icon(
              i <= stars ? Icons.star_rounded : Icons.star_outline_rounded,
              color: i <= stars ? scheme.primary : scheme.outline,
            ),
          ),
      ],
    );
  }
}

class _FactChip extends StatelessWidget {
  const _FactChip({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
      decoration: BoxDecoration(color: scheme.surfaceContainerHigh, borderRadius: BorderRadius.circular(6)),
      child: Text(text, style: TextStyle(fontSize: 12.5, color: scheme.onSurfaceVariant)),
    );
  }
}
