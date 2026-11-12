import 'dart:io';

import 'package:flutter/material.dart';

import '../player_controller.dart';
import '../src/rust/api/library.dart';

String formatMs(int? ms) {
  if (ms == null) return '–:––';
  final s = ms ~/ 1000;
  return '${s ~/ 60}:${(s % 60).toString().padLeft(2, '0')}';
}

/// "3 h 12 min" / "41 min".
String formatLong(int ms) {
  final min = (ms / 60000).round();
  if (min == 0) return ms > 0 ? '< 1 min' : '0 min';
  if (min < 60) return '$min min';
  return '${min ~/ 60} h ${min % 60} min';
}

/// Czech plural: 1 skladba, 2–4 skladby, 5+ skladeb.
String plural(int n, String one, String few, String many) => '$n ${n == 1 ? one : (n >= 2 && n <= 4 ? few : many)}';

String tracksLabel(int n) => plural(n, 'skladba', 'skladby', 'skladeb');

/// A cover image file from the Rust cover cache, or a placeholder when there is none.
class CoverImage extends StatelessWidget {
  const CoverImage({super.key, required this.path, required this.size, this.radius = 6, this.circle = false});

  final String? path;
  final double size;
  final double radius;
  final bool circle;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final placeholder = Container(
      width: size,
      height: size,
      color: scheme.surfaceContainerHighest,
      child: Icon(Icons.album, size: size * 0.5, color: scheme.onSurfaceVariant),
    );
    final p = path;
    final image = p == null
        ? placeholder
        : Image.file(
            File(p),
            width: size,
            height: size,
            fit: BoxFit.cover,
            // Decode at display size, not at the file's size.
            cacheWidth: (size * MediaQuery.devicePixelRatioOf(context)).round(),
            gaplessPlayback: true,
            errorBuilder: (_, _, _) => placeholder,
          );
    return circle ? ClipOval(child: image) : ClipRRect(borderRadius: BorderRadius.circular(radius), child: image);
  }
}

/// Square cover that fills its width (grids, now playing).
class FlexCover extends StatelessWidget {
  const FlexCover({super.key, required this.path, this.radius = 8, this.shadow = false, this.circle = false});

  final String? path;
  final double radius;
  final bool shadow;
  final bool circle;

  @override
  Widget build(BuildContext context) {
    return AspectRatio(
      aspectRatio: 1,
      child: LayoutBuilder(
        builder: (context, c) {
          final cover = CoverImage(path: path, size: c.maxWidth, radius: radius, circle: circle);
          if (!shadow) return cover;
          return DecoratedBox(
            decoration: BoxDecoration(
              borderRadius: BorderRadius.circular(radius),
              boxShadow: const [BoxShadow(color: Color(0x88000000), blurRadius: 40, offset: Offset(0, 20))],
            ),
            child: cover,
          );
        },
      ),
    );
  }
}

/// Track actions: play now, play next, add to queue, go to album.
Future<void> showTrackActions(
  BuildContext context,
  PlayerController player,
  TrackInfo track, {
  Offset? at,
  VoidCallback? openAlbum,
}) async {
  final actions = <(IconData, String, VoidCallback)>[
    (Icons.play_arrow, 'Přehrát', () => player.playNow(track)),
    (Icons.queue_play_next, 'Přehrát další', () => player.playNext([track])),
    (Icons.playlist_add, 'Přidat do fronty', () => player.addToQueue([track])),
    if (openAlbum != null) (Icons.album_outlined, 'Přejít na album', openAlbum),
  ];
  if (at != null) {
    final overlay = Overlay.of(context).context.findRenderObject() as RenderBox;
    final chosen = await showMenu<int>(
      context: context,
      position: RelativeRect.fromRect(at & const Size(1, 1), Offset.zero & overlay.size),
      items: [
        for (final (i, a) in actions.indexed)
          PopupMenuItem(
            value: i,
            child: Row(children: [Icon(a.$1, size: 20), const SizedBox(width: 12), Text(a.$2)]),
          ),
      ],
    );
    if (chosen != null) actions[chosen].$3();
    return;
  }
  await showModalBottomSheet<void>(
    context: context,
    showDragHandle: true,
    builder: (sheet) => SafeArea(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          ListTile(
            leading: CoverImage(path: track.coverSmall, size: 44),
            title: Text(track.title, maxLines: 1, overflow: TextOverflow.ellipsis),
            subtitle: Text(track.artist, maxLines: 1, overflow: TextOverflow.ellipsis),
          ),
          const Divider(),
          for (final a in actions)
            ListTile(
              leading: Icon(a.$1),
              title: Text(a.$2),
              onTap: () {
                Navigator.pop(sheet);
                a.$3();
              },
            ),
        ],
      ),
    ),
  );
}

/// One track in a list. Wide: columns like a table; narrow: two lines.
class TrackRow extends StatelessWidget {
  const TrackRow({
    super.key,
    required this.track,
    required this.player,
    this.number,
    this.showCover = true,
    this.showAlbum = true,
    this.onTap,
    this.openAlbum,
  });

  final TrackInfo track;
  final PlayerController player;
  final int? number;
  final bool showCover;
  final bool showAlbum;
  final VoidCallback? onTap;
  final void Function(int albumId)? openAlbum;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final playing = player.currentTrack?.id == track.id;
    final color = playing ? scheme.primary : scheme.onSurface;
    final muted = playing ? scheme.primary : scheme.onSurfaceVariant;
    final albumAction = track.albumId == null || openAlbum == null ? null : () => openAlbum!(track.albumId!);
    return LayoutBuilder(
      builder: (context, c) {
        final wide = c.maxWidth >= 640;
        final title = Text(
          track.title,
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
          style: TextStyle(color: color, fontWeight: FontWeight.w500),
        );
        final lead = <Widget>[
          if (number != null)
            SizedBox(
              width: 32,
              child: playing
                  ? Icon(Icons.graphic_eq, size: 16, color: scheme.primary)
                  : Text('$number', style: TextStyle(color: scheme.outline)),
            ),
          if (showCover) ...[CoverImage(path: track.coverSmall, size: wide ? 34 : 44), const SizedBox(width: 12)],
        ];
        final body = wide
            ? Row(
                children: [
                  ...lead,
                  Expanded(flex: 4, child: title),
                  Expanded(
                    flex: 3,
                    child: Text(
                      track.artist,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(color: muted),
                    ),
                  ),
                  if (showAlbum)
                    Expanded(
                      flex: 3,
                      child: Text(
                        track.album,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: TextStyle(color: muted),
                      ),
                    ),
                  SizedBox(
                    width: 56,
                    child: Text(
                      formatMs(track.durationMs),
                      textAlign: TextAlign.right,
                      style: TextStyle(color: muted),
                    ),
                  ),
                ],
              )
            : Row(
                children: [
                  ...lead,
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        title,
                        Text(
                          [track.artist, if (showAlbum) track.album].where((s) => s.isNotEmpty).join(' · '),
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: theme.textTheme.bodySmall?.copyWith(color: muted),
                        ),
                      ],
                    ),
                  ),
                  Text(formatMs(track.durationMs), style: theme.textTheme.bodySmall?.copyWith(color: muted)),
                  IconButton(
                    icon: const Icon(Icons.more_vert, size: 20),
                    visualDensity: VisualDensity.compact,
                    onPressed: () => showTrackActions(context, player, track, openAlbum: albumAction),
                  ),
                ],
              );
        return InkWell(
          onTap: track.missing ? null : (onTap ?? () => player.playNow(track)),
          onSecondaryTapUp: (d) =>
              showTrackActions(context, player, track, at: d.globalPosition, openAlbum: albumAction),
          onLongPress: () => showTrackActions(context, player, track, openAlbum: albumAction),
          borderRadius: BorderRadius.circular(6),
          child: Opacity(
            opacity: track.missing ? .4 : 1,
            child: Padding(
              padding: EdgeInsets.symmetric(horizontal: 8, vertical: wide ? 5 : 6),
              child: body,
            ),
          ),
        );
      },
    );
  }
}

/// Large page title with a muted subtitle.
class PageHeader extends StatelessWidget {
  const PageHeader({super.key, required this.title, this.subtitle, this.trailing});

  final String title;
  final String? subtitle;
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    final t = Theme.of(context).textTheme;
    return Padding(
      padding: const EdgeInsets.fromLTRB(4, 8, 4, 12),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.end,
        children: [
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(title, style: t.headlineSmall?.copyWith(fontWeight: FontWeight.w700)),
                if (subtitle != null)
                  Text(subtitle!, style: t.bodyMedium?.copyWith(color: Theme.of(context).colorScheme.onSurfaceVariant)),
              ],
            ),
          ),
          ?trailing,
        ],
      ),
    );
  }
}

/// Centered message for empty states.
class EmptyState extends StatelessWidget {
  const EmptyState({super.key, required this.icon, required this.text, this.action});

  final IconData icon;
  final String text;
  final Widget? action;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(24),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(icon, size: 48, color: scheme.outline),
            const SizedBox(height: 12),
            Text(
              text,
              textAlign: TextAlign.center,
              style: TextStyle(color: scheme.onSurfaceVariant),
            ),
            if (action != null) ...[const SizedBox(height: 16), action!],
          ],
        ),
      ),
    );
  }
}
