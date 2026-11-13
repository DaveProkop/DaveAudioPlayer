import 'package:flutter/material.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge.dart' show Int64List;

import '../player_controller.dart';
import '../src/rust/api/library.dart';
import '../src/rust/api/playlists.dart';
import 'library_pages.dart' show LibraryFuture, openAlbum;
import 'widgets.dart';

void openPlaylist(BuildContext context, PlayerController player, int playlistId) {
  Navigator.of(context).push(MaterialPageRoute(builder: (_) => PlaylistPage(player: player, playlistId: playlistId)));
}

/// Asks for a playlist name; `null` when cancelled or empty.
Future<String?> promptPlaylistName(BuildContext context, {String initial = '', String title = 'Nový playlist'}) async {
  final controller = TextEditingController(text: initial);
  final name = await showDialog<String>(
    context: context,
    builder: (dialog) => AlertDialog(
      title: Text(title),
      content: TextField(
        controller: controller,
        autofocus: true,
        decoration: const InputDecoration(labelText: 'Název'),
        onSubmitted: (v) => Navigator.of(dialog).pop(v),
      ),
      actions: [
        TextButton(onPressed: () => Navigator.of(dialog).pop(), child: const Text('Zrušit')),
        FilledButton(onPressed: () => Navigator.of(dialog).pop(controller.text), child: const Text('Uložit')),
      ],
    ),
  );
  final trimmed = name?.trim() ?? '';
  return trimmed.isEmpty ? null : trimmed;
}

/// Creates a playlist from `trackIds` after asking for its name.
Future<void> saveAsPlaylist(BuildContext context, PlayerController player, List<int> trackIds) async {
  final name = await promptPlaylistName(context);
  if (name == null) return;
  await createPlaylist(name: name, trackIds: Int64List.fromList(trackIds));
  player.libraryEdited();
  player.messages.add('Playlist „$name“ uložen');
}

/// Lets the user pick a playlist (or create one) and adds `tracks` to it.
Future<void> showAddToPlaylist(BuildContext context, PlayerController player, List<TrackInfo> tracks) async {
  if (tracks.isEmpty) return;
  final list = await playlists();
  if (!context.mounted) return;
  // -1 = a new playlist.
  final chosen = await showDialog<int>(
    context: context,
    builder: (dialog) => SimpleDialog(
      title: Text(tracks.length == 1 ? 'Přidat do playlistu' : 'Přidat ${tracksLabel(tracks.length)} do playlistu'),
      children: [
        SimpleDialogOption(
          onPressed: () => Navigator.of(dialog).pop(-1),
          child: const ListTile(leading: Icon(Icons.add), title: Text('Nový playlist…')),
        ),
        for (final p in list)
          SimpleDialogOption(
            onPressed: () => Navigator.of(dialog).pop(p.id),
            child: ListTile(
              leading: CoverImage(path: p.coverMedium, size: 40),
              title: Text(p.name),
              subtitle: Text(tracksLabel(p.trackCount)),
            ),
          ),
      ],
    ),
  );
  if (chosen == null || !context.mounted) return;
  final ids = Int64List.fromList([for (final t in tracks) t.id]);
  if (chosen == -1) {
    await saveAsPlaylist(context, player, [for (final t in tracks) t.id]);
    return;
  }
  await addToPlaylist(playlistId: chosen, trackIds: ids);
  player.libraryEdited();
  final name = list.firstWhere((p) => p.id == chosen).name;
  player.messages.add(tracks.length == 1 ? 'Přidáno do „$name“' : '${tracksLabel(tracks.length)} přidáno do „$name“');
}

/// All playlists as a grid.
class PlaylistsPage extends StatelessWidget {
  const PlaylistsPage({super.key, required this.player, this.header});

  final PlayerController player;
  final Widget? header;

  @override
  Widget build(BuildContext context) {
    final t = Theme.of(context);
    return LibraryFuture<List<PlaylistInfo>>(
      player: player,
      load: playlists,
      builder: (context, list) => CustomScrollView(
        slivers: [
          SliverPadding(
            padding: const EdgeInsets.fromLTRB(16, 12, 16, 0),
            sliver: SliverToBoxAdapter(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  ?header,
                  PageHeader(
                    title: 'Playlisty',
                    subtitle: plural(list.length, 'playlist', 'playlisty', 'playlistů'),
                    trailing: FilledButton.tonalIcon(
                      onPressed: () => saveAsPlaylist(context, player, const []),
                      icon: const Icon(Icons.add),
                      label: const Text('Nový'),
                    ),
                  ),
                ],
              ),
            ),
          ),
          if (list.isEmpty)
            const SliverFillRemaining(
              hasScrollBody: false,
              child: EmptyState(
                icon: Icons.queue_music,
                text: 'Zatím žádné playlisty.\nPřidej skladby z knihovny nebo ulož frontu.',
              ),
            )
          else
            SliverPadding(
              padding: const EdgeInsets.fromLTRB(16, 0, 16, 24),
              sliver: SliverGrid.builder(
                gridDelegate: const SliverGridDelegateWithMaxCrossAxisExtent(
                  maxCrossAxisExtent: 190,
                  mainAxisSpacing: 18,
                  crossAxisSpacing: 18,
                  childAspectRatio: .76,
                ),
                itemCount: list.length,
                itemBuilder: (context, i) {
                  final p = list[i];
                  return InkWell(
                    borderRadius: BorderRadius.circular(8),
                    onTap: () => openPlaylist(context, player, p.id),
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        FlexCover(path: p.coverMedium),
                        const SizedBox(height: 8),
                        Text(
                          p.name,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: const TextStyle(fontWeight: FontWeight.w600),
                        ),
                        Text(
                          '${tracksLabel(p.trackCount)} · ${formatLong(p.durationMs)}',
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: t.textTheme.bodySmall?.copyWith(color: t.colorScheme.onSurfaceVariant),
                        ),
                      ],
                    ),
                  );
                },
              ),
            ),
        ],
      ),
    );
  }
}

/// One playlist: play it, rename or delete it, reorder and remove its tracks.
class PlaylistPage extends StatelessWidget {
  const PlaylistPage({super.key, required this.player, required this.playlistId});

  final PlayerController player;
  final int playlistId;

  Future<void> _rename(BuildContext context, PlaylistInfo info) async {
    final name = await promptPlaylistName(context, initial: info.name, title: 'Přejmenovat playlist');
    if (name == null) return;
    await renamePlaylist(playlistId: playlistId, name: name);
    player.libraryEdited();
  }

  Future<void> _delete(BuildContext context, PlaylistInfo info) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (dialog) => AlertDialog(
        title: Text('Smazat „${info.name}“?'),
        content: const Text('Smaže se jen playlist, skladby v knihovně zůstanou.'),
        actions: [
          TextButton(onPressed: () => Navigator.of(dialog).pop(false), child: const Text('Zrušit')),
          FilledButton(onPressed: () => Navigator.of(dialog).pop(true), child: const Text('Smazat')),
        ],
      ),
    );
    if (ok != true || !context.mounted) return;
    await deletePlaylist(playlistId: playlistId);
    player.libraryEdited();
    if (context.mounted) Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Scaffold(
      backgroundColor: Colors.transparent,
      body: LibraryFuture<(PlaylistInfo?, List<PlaylistTrack>)>(
        player: player,
        load: () async {
          final all = await playlists();
          final info = all.where((p) => p.id == playlistId).firstOrNull;
          return (info, info == null ? <PlaylistTrack>[] : await playlistTracks(playlistId: playlistId));
        },
        builder: (context, data) {
          final (info, entries) = data;
          if (info == null) return const EmptyState(icon: Icons.queue_music, text: 'Playlist už neexistuje');
          final tracks = [for (final e in entries) e.track];
          player.remember(tracks);
          final header = Container(
            decoration: BoxDecoration(
              gradient: LinearGradient(
                begin: Alignment.topCenter,
                end: Alignment.bottomCenter,
                colors: [scheme.primary.withValues(alpha: .22), Colors.transparent],
              ),
            ),
            padding: const EdgeInsets.fromLTRB(20, 8, 20, 18),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                if (Navigator.of(context).canPop())
                  TextButton.icon(
                    onPressed: () => Navigator.of(context).pop(),
                    icon: const Icon(Icons.arrow_back),
                    label: const Text('Zpět'),
                  ),
                const SizedBox(height: 8),
                Row(
                  crossAxisAlignment: CrossAxisAlignment.end,
                  children: [
                    CoverImage(path: info.coverMedium, size: 140, radius: 10),
                    const SizedBox(width: 20),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(
                            'PLAYLIST',
                            style: theme.textTheme.labelSmall?.copyWith(
                              color: scheme.onSurfaceVariant,
                              letterSpacing: .8,
                            ),
                          ),
                          Text(info.name, style: theme.textTheme.headlineMedium?.copyWith(fontWeight: FontWeight.w700)),
                          const SizedBox(height: 4),
                          Text(
                            '${tracksLabel(info.trackCount)} · ${formatLong(info.durationMs)}',
                            style: TextStyle(color: scheme.onSurfaceVariant),
                          ),
                        ],
                      ),
                    ),
                  ],
                ),
                const SizedBox(height: 14),
                Wrap(
                  spacing: 8,
                  runSpacing: 8,
                  crossAxisAlignment: WrapCrossAlignment.center,
                  children: [
                    FilledButton.icon(
                      onPressed: tracks.isEmpty ? null : () => player.playAll(tracks),
                      icon: const Icon(Icons.play_arrow),
                      label: const Text('Přehrát'),
                    ),
                    FilledButton.tonalIcon(
                      onPressed: tracks.isEmpty ? null : () => player.addToQueue(tracks),
                      icon: const Icon(Icons.playlist_add),
                      label: const Text('Do fronty'),
                    ),
                    IconButton(
                      tooltip: 'Přejmenovat',
                      onPressed: () => _rename(context, info),
                      icon: const Icon(Icons.edit_outlined),
                    ),
                    IconButton(
                      tooltip: 'Smazat playlist',
                      onPressed: () => _delete(context, info),
                      icon: const Icon(Icons.delete_outline),
                    ),
                  ],
                ),
              ],
            ),
          );
          return ListenableBuilder(
            listenable: player,
            builder: (context, _) => CustomScrollView(
              slivers: [
                SliverToBoxAdapter(child: header),
                if (entries.isEmpty)
                  const SliverFillRemaining(
                    hasScrollBody: false,
                    child: EmptyState(
                      icon: Icons.playlist_add,
                      text: 'Playlist je prázdný.\nSkladby přidáš z knihovny (Přidat do playlistu).',
                    ),
                  )
                else
                  SliverPadding(
                    padding: const EdgeInsets.fromLTRB(12, 0, 12, 24),
                    sliver: SliverReorderableList(
                      itemCount: entries.length,
                      // onReorderItem reports the final index, as move_playlist_item expects.
                      onReorderItem: (from, to) async {
                        if (to == from) return;
                        await movePlaylistItem(itemId: entries[from].itemId, to: to);
                        player.libraryEdited();
                      },
                      itemBuilder: (context, i) {
                        final e = entries[i];
                        return Dismissible(
                          key: ValueKey(e.itemId),
                          direction: DismissDirection.endToStart,
                          background: Container(
                            alignment: Alignment.centerRight,
                            padding: const EdgeInsets.only(right: 16),
                            color: scheme.errorContainer,
                            child: const Icon(Icons.delete_outline),
                          ),
                          onDismissed: (_) async {
                            await removeFromPlaylist(itemIds: Int64List.fromList([e.itemId]));
                            player.libraryEdited();
                          },
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
                              Expanded(
                                child: TrackRow(
                                  track: e.track,
                                  player: player,
                                  onTap: () => player.playAll(tracks, start: i),
                                  openAlbum: (id) => openAlbum(context, player, id),
                                ),
                              ),
                            ],
                          ),
                        );
                      },
                    ),
                  ),
              ],
            ),
          );
        },
      ),
    );
  }
}
