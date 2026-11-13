import 'dart:async';

import 'package:flutter/material.dart';

import '../player_controller.dart';
import '../src/rust/api/library.dart';
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

  @override
  Widget build(BuildContext context) {
    final p = widget.player;
    final scheme = Theme.of(context).colorScheme;
    return PositionTicker(
      player: p,
      builder: (context) {
        final duration = p.state?.durationMs ?? 0;
        final pos = _drag ?? p.positionMs.toDouble();
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
                  setState(() => _drag = null);
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
                        label: (s?.crossfadeMs ?? 0) == 0 ? 'Gapless' : '✕ ${(s!.crossfadeMs / 1000).round()} s',
                        on: (s?.crossfadeMs ?? 0) > 0,
                        tooltip: 'Crossfade (nastavení délky v Nastavení)',
                        onTap: s == null ? null : () => rust.setCrossfade(ms: s.crossfadeMs == 0 ? 6000 : 0),
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
        final info = Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(t.title, style: theme.textTheme.headlineMedium?.copyWith(fontWeight: FontWeight.w700)),
            const SizedBox(height: 2),
            Text(
              [
                t.artist,
                if (t.album.isNotEmpty) t.album + (details?.year != null ? ' (${details!.year})' : ''),
              ].join(' · '),
              style: theme.textTheme.titleMedium?.copyWith(color: scheme.onSurfaceVariant),
            ),
            const SizedBox(height: 14),
            Wrap(
              spacing: 8,
              runSpacing: 8,
              children: [
                for (final f in facts)
                  Container(
                    padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
                    decoration: BoxDecoration(
                      color: scheme.surfaceContainerHigh,
                      borderRadius: BorderRadius.circular(6),
                    ),
                    child: Text(f, style: TextStyle(fontSize: 12.5, color: scheme.onSurfaceVariant)),
                  ),
              ],
            ),
            const SizedBox(height: 16),
            SeekBar(player: player, compact: true),
            const SizedBox(height: 8),
            Center(child: TransportButtons(player: player, large: true)),
            if (next != null) ...[
              const SizedBox(height: 20),
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
                final body = wide
                    ? Row(
                        children: [
                          Expanded(flex: 5, child: Center(child: cover)),
                          const SizedBox(width: 36),
                          Expanded(flex: 6, child: info),
                        ],
                      )
                    : Column(children: [cover, const SizedBox(height: 20), info]);
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

/// How the current track passes into the next one, and in how long.
class _TransitionLine extends StatelessWidget {
  const _TransitionLine({required this.player, required this.gapless});

  final PlayerController player;
  final bool gapless;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final tr = player.state?.transition;
    final s = player.settings;
    final label = switch (tr?.kind) {
      rust.TransitionKindDto.beatmatch => 'beatmatch – srovnání do rytmu',
      rust.TransitionKindDto.filterSweep => 'přechod filtrem',
      rust.TransitionKindDto.echoOut => 'echo na poslední době',
      rust.TransitionKindDto.hardCut => 'střih na dobu',
      rust.TransitionKindDto.crossfade => 'prolnutí ${((s?.crossfadeMs ?? 0) / 1000).round()} s',
      rust.TransitionKindDto.gapless => gapless ? 'bez mezery – další skladba alba' : 'bez mezery',
      null =>
        gapless
            ? 'bez mezery – další skladba alba'
            : (s?.crossfadeMs ?? 0) > 0
            ? 'prolnutí ${(s!.crossfadeMs / 1000).round()} s'
            : 'bez mezery',
    };
    final style = TextStyle(fontSize: 12.5, color: gapless ? context.appColors.gapless : scheme.primary);
    if (tr == null) return Text(label, style: style);
    return PositionTicker(
      player: player,
      builder: (context) {
        final left = tr.startMs - player.positionMs;
        return Text(left > 0 ? '$label · za ${formatMs(left)}' : label, style: style);
      },
    );
  }
}
