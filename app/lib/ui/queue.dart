import 'package:flutter/material.dart';

import '../player_controller.dart';
import '../src/rust/api/player.dart' as rust;
import 'theme.dart';
import 'widgets.dart';

/// The play queue: played (dimmed), now playing, next. Drag by the handle to reorder,
/// swipe left to remove. Between upcoming items a small label says how the tracks join.
class QueueView extends StatelessWidget {
  const QueueView({super.key, required this.player, this.showHeader = true});

  final PlayerController player;
  final bool showHeader;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return ListenableBuilder(
      listenable: player,
      builder: (context, _) {
        final queue = player.queue;
        final current = player.currentIndex;
        final remainingMs = [
          for (var i = current ?? 0; i < queue.length; i++) player.track(queue[i].trackId)?.durationMs ?? 0,
        ].fold<int>(0, (a, b) => a + b);
        final crossfadeMs = player.settings?.crossfadeMs ?? 0;
        return Column(
          children: [
            if (showHeader)
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 14, 8, 6),
                child: Row(
                  children: [
                    Text('Fronta', style: theme.textTheme.titleMedium?.copyWith(fontWeight: FontWeight.w700)),
                    const SizedBox(width: 10),
                    Expanded(
                      child: Text(
                        queue.isEmpty ? '' : '${tracksLabel(queue.length)} · ${formatLong(remainingMs)}',
                        style: TextStyle(fontSize: 12.5, color: scheme.onSurfaceVariant),
                      ),
                    ),
                    IconButton(
                      tooltip: 'Vyprázdnit frontu',
                      icon: const Icon(Icons.clear_all),
                      onPressed: queue.isEmpty ? null : () => rust.clearQueue(),
                    ),
                  ],
                ),
              ),
            Expanded(
              child: queue.isEmpty
                  ? const EmptyState(icon: Icons.queue_music, text: 'Fronta je prázdná.\nPřidej skladby z knihovny.')
                  : ReorderableListView.builder(
                      padding: const EdgeInsets.fromLTRB(8, 0, 8, 12),
                      buildDefaultDragHandles: false,
                      itemCount: queue.length,
                      // onReorderItem already reports the final index, as the engine expects.
                      onReorderItem: (from, to) => rust.moveQueueItem(from: from, to: to),
                      itemBuilder: (context, i) {
                        final item = queue[i];
                        final t = player.track(item.trackId);
                        final isNow = current == i;
                        final played = current != null && i < current;
                        String? section;
                        if (i == 0 && played) section = 'Přehráno';
                        if (isNow) section = 'Právě hraje';
                        if (current != null && i == current + 1) section = 'Další';
                        String? join;
                        if (current != null && i > current) {
                          final prev = player.track(queue[i - 1].trackId);
                          join = PlayerController.continuesAlbum(prev, t) ? 'album' : (crossfadeMs > 0 ? 'fade' : null);
                        }
                        return Dismissible(
                          key: ValueKey(item.id),
                          direction: DismissDirection.endToStart,
                          background: Container(
                            alignment: Alignment.centerRight,
                            padding: const EdgeInsets.only(right: 16),
                            color: scheme.errorContainer,
                            child: const Icon(Icons.delete_outline),
                          ),
                          onDismissed: (_) => rust.removeQueueItem(index: i),
                          child: Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              if (section != null)
                                Padding(
                                  padding: const EdgeInsets.fromLTRB(8, 10, 8, 6),
                                  child: Text(
                                    section.toUpperCase(),
                                    style: TextStyle(fontSize: 11, letterSpacing: .8, color: scheme.outline),
                                  ),
                                ),
                              if (join != null && section == null)
                                Padding(
                                  padding: const EdgeInsets.only(left: 64, bottom: 2),
                                  child: Text(
                                    join == 'album'
                                        ? '▬ bez mezery (album)'
                                        : '✕ prolnutí ${(crossfadeMs / 1000).round()} s',
                                    style: TextStyle(
                                      fontSize: 11.5,
                                      color: join == 'album' ? context.appColors.gapless : scheme.outline,
                                    ),
                                  ),
                                ),
                              Opacity(
                                opacity: played ? .45 : 1,
                                child: Material(
                                  color: isNow ? scheme.primary.withValues(alpha: .16) : Colors.transparent,
                                  borderRadius: BorderRadius.circular(8),
                                  child: InkWell(
                                    borderRadius: BorderRadius.circular(8),
                                    onTap: () => rust.playIndex(index: i),
                                    child: Padding(
                                      padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 6),
                                      child: Row(
                                        children: [
                                          ReorderableDragStartListener(
                                            index: i,
                                            child: MouseRegion(
                                              cursor: SystemMouseCursors.grab,
                                              child: Padding(
                                                padding: const EdgeInsets.symmetric(horizontal: 4),
                                                child: Icon(Icons.drag_indicator, size: 18, color: scheme.outline),
                                              ),
                                            ),
                                          ),
                                          CoverImage(path: t?.coverSmall, size: 38),
                                          const SizedBox(width: 10),
                                          Expanded(
                                            child: Column(
                                              crossAxisAlignment: CrossAxisAlignment.start,
                                              children: [
                                                Text(
                                                  t?.title ?? '…',
                                                  maxLines: 1,
                                                  overflow: TextOverflow.ellipsis,
                                                  style: TextStyle(
                                                    fontWeight: FontWeight.w500,
                                                    color: isNow ? scheme.primary : null,
                                                  ),
                                                ),
                                                Text(
                                                  t?.artist ?? '',
                                                  maxLines: 1,
                                                  overflow: TextOverflow.ellipsis,
                                                  style: TextStyle(fontSize: 12.5, color: scheme.onSurfaceVariant),
                                                ),
                                              ],
                                            ),
                                          ),
                                          Padding(
                                            padding: const EdgeInsets.only(left: 8, right: 4),
                                            child: Row(
                                              mainAxisSize: MainAxisSize.min,
                                              children: [
                                                // Each upcoming key compared with the one before it.
                                                KeyBadge(
                                                  musicKey: t?.key,
                                                  reference: current != null && i > current
                                                      ? player.track(queue[i - 1].trackId)?.key
                                                      : null,
                                                ),
                                                const SizedBox(width: 8),
                                                isNow
                                                    ? Icon(Icons.graphic_eq, size: 16, color: scheme.primary)
                                                    : Text(
                                                        formatMs(t?.durationMs),
                                                        style: TextStyle(fontSize: 12.5, color: scheme.outline),
                                                      ),
                                              ],
                                            ),
                                          ),
                                        ],
                                      ),
                                    ),
                                  ),
                                ),
                              ),
                            ],
                          ),
                        );
                      },
                    ),
            ),
          ],
        );
      },
    );
  }
}
