import 'package:flutter/material.dart';

import '../player_controller.dart';
import '../src/rust/api/folders.dart';
import 'library_pages.dart' show LibraryFuture, openAlbum;
import 'widgets.dart';

/// The library as it lies on disk: library folders, their subfolders and tracks.
class FoldersPage extends StatefulWidget {
  const FoldersPage({super.key, required this.player, this.header});

  final PlayerController player;
  final Widget? header;

  @override
  State<FoldersPage> createState() => _FoldersPageState();
}

class _FoldersPageState extends State<FoldersPage> {
  /// Where we are: the library folder (null = all of them), its display name and the
  /// path below it.
  int? _root;
  String _rootName = '';
  String _dir = '';

  void _open({int? root, String rootName = '', String dir = ''}) => setState(() {
    _root = root;
    _rootName = rootName;
    _dir = dir;
  });

  /// One level up.
  void _up() {
    if (_dir.isEmpty) return _open();
    final i = _dir.lastIndexOf('/');
    _open(root: _root, rootName: _rootName, dir: i < 0 ? '' : _dir.substring(0, i));
  }

  Future<void> _playFolder({bool queue = false}) async {
    final root = _root;
    if (root == null) return;
    final tracks = await folderTracks(rootId: root, dir: _dir);
    if (tracks.isEmpty) return;
    queue ? await widget.player.addToQueue(tracks) : await widget.player.playAll(tracks);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final root = _root;
    final crumbs = [
      ('Složky', () => _open()),
      if (root != null) (_rootName.split(RegExp(r'[\\/]')).where((s) => s.isNotEmpty).lastOrNull ?? _rootName, () => _open(root: root, rootName: _rootName)),
      if (root != null && _dir.isNotEmpty)
        for (final (i, part) in _dir.split('/').indexed)
          (part, () => _open(root: root, rootName: _rootName, dir: _dir.split('/').take(i + 1).join('/'))),
    ];
    return PopScope(
      canPop: root == null,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop) _up();
      },
      child: LibraryFuture<FolderListing>(
        key: ValueKey('$root/$_dir'),
        player: widget.player,
        load: () => folderListing(rootId: root, dir: _dir),
        builder: (context, listing) => CustomScrollView(
          slivers: [
            SliverPadding(
              padding: const EdgeInsets.fromLTRB(16, 12, 16, 4),
              sliver: SliverToBoxAdapter(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    ?widget.header,
                    Wrap(
                      crossAxisAlignment: WrapCrossAlignment.center,
                      children: [
                        for (final (i, (label, go)) in crumbs.indexed) ...[
                          if (i > 0) Icon(Icons.chevron_right, size: 18, color: scheme.outline),
                          TextButton(
                            onPressed: i == crumbs.length - 1 ? null : go,
                            child: Text(
                              label,
                              style: i == crumbs.length - 1
                                  ? theme.textTheme.titleLarge?.copyWith(fontWeight: FontWeight.w700)
                                  : null,
                            ),
                          ),
                        ],
                      ],
                    ),
                    if (root != null)
                      Padding(
                        padding: const EdgeInsets.only(top: 4, bottom: 8),
                        child: Wrap(
                          spacing: 8,
                          children: [
                            FilledButton.icon(
                              onPressed: () => _playFolder(),
                              icon: const Icon(Icons.play_arrow),
                              label: const Text('Přehrát složku'),
                            ),
                            FilledButton.tonalIcon(
                              onPressed: () => _playFolder(queue: true),
                              icon: const Icon(Icons.playlist_add),
                              label: const Text('Do fronty'),
                            ),
                          ],
                        ),
                      ),
                  ],
                ),
              ),
            ),
            if (listing.folders.isEmpty && listing.tracks.isEmpty)
              const SliverFillRemaining(
                hasScrollBody: false,
                child: EmptyState(icon: Icons.folder_off_outlined, text: 'Tady nic není'),
              ),
            SliverPadding(
              padding: const EdgeInsets.symmetric(horizontal: 12),
              sliver: SliverList.builder(
                itemCount: listing.folders.length,
                itemBuilder: (context, i) {
                  final f = listing.folders[i];
                  return ListTile(
                    leading: Icon(Icons.folder_outlined, color: scheme.primary),
                    title: Text(f.name, maxLines: 1, overflow: TextOverflow.ellipsis),
                    subtitle: Text(tracksLabel(f.trackCount)),
                    trailing: const Icon(Icons.chevron_right),
                    onTap: () => root == null
                        ? _open(root: listing.rootIds[i].toInt(), rootName: f.name)
                        : _open(root: root, rootName: _rootName, dir: f.dir),
                  );
                },
              ),
            ),
            SliverPadding(
              padding: const EdgeInsets.fromLTRB(12, 0, 12, 24),
              sliver: SliverList.builder(
                itemCount: listing.tracks.length,
                itemBuilder: (context, i) => TrackRow(
                  track: listing.tracks[i],
                  player: widget.player,
                  onTap: () => widget.player.playAll(listing.tracks, start: i),
                  openAlbum: (id) => openAlbum(context, widget.player, id),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
