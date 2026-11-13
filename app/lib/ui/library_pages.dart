import 'dart:async';

import 'package:flutter/material.dart';

import '../player_controller.dart';
import '../src/rust/api/library.dart';
import 'playlists.dart' show showAddToPlaylist;
import '../src/rust/api/metadata.dart' show findAlbumCover;
import 'folder_picker.dart';
import 'widgets.dart';

/// Opens album and artist details inside the content area's navigator.
void openAlbum(BuildContext context, PlayerController player, int albumId) {
  Navigator.of(context).push(
    MaterialPageRoute(
      builder: (_) => AlbumPage(player: player, albumId: albumId),
    ),
  );
}

void openArtist(BuildContext context, PlayerController player, ArtistInfo artist) {
  Navigator.of(context).push(
    MaterialPageRoute(
      builder: (_) => ArtistPage(player: player, artist: artist),
    ),
  );
}

/// Reloads its future whenever the library changes (after a scan).
class LibraryFuture<T> extends StatefulWidget {
  const LibraryFuture({super.key, required this.player, required this.load, required this.builder});

  final PlayerController player;
  final Future<T> Function() load;
  final Widget Function(BuildContext, T) builder;

  @override
  State<LibraryFuture<T>> createState() => _LibraryFutureState<T>();
}

class _LibraryFutureState<T> extends State<LibraryFuture<T>> {
  late Future<T> _future;
  late int _version;

  @override
  void initState() {
    super.initState();
    _version = widget.player.libraryVersion;
    _future = widget.load();
    widget.player.addListener(_changed);
  }

  void _changed() {
    if (widget.player.libraryVersion != _version) {
      _version = widget.player.libraryVersion;
      // A block body: an arrow would return the Future, which setState rejects.
      setState(() {
        _future = widget.load();
      });
    }
  }

  @override
  void dispose() {
    widget.player.removeListener(_changed);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return FutureBuilder<T>(
      future: _future,
      builder: (context, snap) {
        if (snap.hasError) return EmptyState(icon: Icons.error_outline, text: 'Nelze načíst knihovnu\n${snap.error}');
        if (!snap.hasData) return const Center(child: CircularProgressIndicator());
        return widget.builder(context, snap.data as T);
      },
    );
  }
}

// ----- tracks ---------------------------------------------------------------------------

/// All tracks, loaded page by page while scrolling, with search on top.
class TracksPage extends StatefulWidget {
  const TracksPage({super.key, required this.player, this.header, this.autofocusSearch = false});

  final PlayerController player;
  final Widget? header;
  final bool autofocusSearch;

  @override
  State<TracksPage> createState() => _TracksPageState();
}

class _TracksPageState extends State<TracksPage> {
  static const _page = 200;
  final _pages = <int, List<TrackInfo>>{};
  final _loading = <int>{};

  /// Pages from before a refresh, shown until their fresh copy arrives (no flicker while
  /// the analysis adds keys).
  var _stale = <int, List<TrackInfo>>{};
  int? _count;
  LibraryStats? _stats;
  int _version = -1;

  final _query = TextEditingController();
  Timer? _debounce;
  List<TrackInfo>? _results;

  PlayerController get player => widget.player;

  @override
  void initState() {
    super.initState();
    player.addListener(_changed);
    _reload();
  }

  @override
  void dispose() {
    player.removeListener(_changed);
    _debounce?.cancel();
    _query.dispose();
    super.dispose();
  }

  void _changed() {
    if (player.libraryVersion != _version) _reload();
  }

  Future<void> _reload() async {
    _version = player.libraryVersion;
    _stale = {..._stale, ..._pages};
    _pages.clear();
    _loading.clear();
    try {
      final stats = await libraryStats();
      if (!mounted) return;
      setState(() {
        _stats = stats;
        _count = stats.tracks;
      });
      if (_query.text.trim().isNotEmpty) _search(_query.text);
    } catch (_) {
      if (mounted) setState(() => _count = 0);
    }
  }

  TrackInfo? _at(int i) {
    final page = i ~/ _page;
    final rows = _pages[page];
    if (rows != null) return i % _page < rows.length ? rows[i % _page] : null;
    final old = _stale[page];
    if (_loading.add(page)) {
      final version = _version;
      libraryPage(offset: page * _page, limit: _page).then(
        (rows) {
          if (!mounted || version != _version) return;
          player.remember(rows);
          setState(() {
            _pages[page] = rows;
            _stale.remove(page);
          });
        },
        onError: (_) {
          _loading.remove(page);
        },
      );
    }
    return old != null && i % _page < old.length ? old[i % _page] : null;
  }

  void _search(String q) {
    _debounce?.cancel();
    if (q.trim().isEmpty) {
      setState(() => _results = null);
      return;
    }
    _debounce = Timer(const Duration(milliseconds: 180), () async {
      final hits = await search(query: q, limit: 300);
      if (!mounted || _query.text != q) return;
      player.remember(hits);
      setState(() => _results = hits);
    });
  }

  @override
  Widget build(BuildContext context) {
    final results = _results;
    final count = results?.length ?? _count;
    final stats = _stats;
    return ListenableBuilder(
      listenable: player,
      builder: (context, _) => CustomScrollView(
        slivers: [
          SliverPadding(
            padding: const EdgeInsets.fromLTRB(16, 12, 16, 0),
            sliver: SliverToBoxAdapter(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  ?widget.header,
                  SearchField(controller: _query, onChanged: _search, autofocus: widget.autofocusSearch),
                  PageHeader(
                    title: results == null ? 'Skladby' : 'Výsledky hledání',
                    subtitle: results != null
                        ? tracksLabel(results.length)
                        : stats == null
                        ? null
                        : '${tracksLabel(stats.tracks)} · ${formatLong(stats.totalMs)}',
                  ),
                ],
              ),
            ),
          ),
          if (count == null)
            const SliverFillRemaining(child: Center(child: CircularProgressIndicator()))
          else if (count == 0)
            SliverFillRemaining(
              hasScrollBody: false,
              child: EmptyState(
                icon: results == null ? Icons.library_music_outlined : Icons.search_off,
                text: results == null ? 'Knihovna je prázdná.' : 'Nic nenalezeno',
                action: results == null
                    ? FilledButton.icon(
                        onPressed: () => addMusicFolders(context, player),
                        icon: const Icon(Icons.create_new_folder_outlined),
                        label: const Text('Přidat hudbu'),
                      )
                    : null,
              ),
            )
          else
            SliverPadding(
              padding: const EdgeInsets.fromLTRB(8, 0, 8, 16),
              sliver: SliverFixedExtentList.builder(
                itemExtent: 52,
                itemCount: count,
                itemBuilder: (context, i) {
                  final t = results != null ? results[i] : _at(i);
                  if (t == null) return const SizedBox.shrink();
                  return TrackRow(track: t, player: player, openAlbum: (id) => openAlbum(context, player, id));
                },
              ),
            ),
        ],
      ),
    );
  }
}

class SearchField extends StatelessWidget {
  const SearchField({super.key, required this.controller, required this.onChanged, this.autofocus = false, this.hint});

  final TextEditingController controller;
  final ValueChanged<String> onChanged;
  final bool autofocus;
  final String? hint;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return ConstrainedBox(
      constraints: const BoxConstraints(maxWidth: 560),
      child: TextField(
        controller: controller,
        autofocus: autofocus,
        onChanged: onChanged,
        decoration: InputDecoration(
          hintText: hint ?? 'Hledat skladby, interprety, alba…',
          prefixIcon: const Icon(Icons.search),
          suffixIcon: ListenableBuilder(
            listenable: controller,
            builder: (_, _) => controller.text.isEmpty
                ? const SizedBox.shrink()
                : IconButton(
                    icon: const Icon(Icons.close),
                    onPressed: () {
                      controller.clear();
                      onChanged('');
                    },
                  ),
          ),
          filled: true,
          fillColor: scheme.surfaceContainerHigh,
          isDense: true,
          border: OutlineInputBorder(borderRadius: BorderRadius.circular(999), borderSide: BorderSide.none),
        ),
      ),
    );
  }
}

// ----- albums ---------------------------------------------------------------------------

class AlbumsPage extends StatelessWidget {
  const AlbumsPage({super.key, required this.player, this.header});

  final PlayerController player;
  final Widget? header;

  @override
  Widget build(BuildContext context) {
    return LibraryFuture<List<AlbumInfo>>(
      player: player,
      load: albums,
      builder: (context, list) => CustomScrollView(
        slivers: [
          SliverPadding(
            padding: const EdgeInsets.fromLTRB(16, 12, 16, 0),
            sliver: SliverToBoxAdapter(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  ?header,
                  PageHeader(title: 'Alba', subtitle: plural(list.length, 'album', 'alba', 'alb')),
                ],
              ),
            ),
          ),
          if (list.isEmpty)
            const SliverFillRemaining(
              hasScrollBody: false,
              child: EmptyState(icon: Icons.album_outlined, text: 'Zatím žádná alba'),
            )
          else
            AlbumGrid(albums: list, player: player),
        ],
      ),
    );
  }
}

class AlbumGrid extends StatelessWidget {
  const AlbumGrid({super.key, required this.albums, required this.player});

  final List<AlbumInfo> albums;
  final PlayerController player;

  @override
  Widget build(BuildContext context) {
    final t = Theme.of(context);
    return SliverPadding(
      padding: const EdgeInsets.fromLTRB(16, 0, 16, 24),
      sliver: SliverGrid.builder(
        gridDelegate: const SliverGridDelegateWithMaxCrossAxisExtent(
          maxCrossAxisExtent: 190,
          mainAxisSpacing: 18,
          crossAxisSpacing: 18,
          childAspectRatio: .76,
        ),
        itemCount: albums.length,
        itemBuilder: (context, i) {
          final a = albums[i];
          return InkWell(
            borderRadius: BorderRadius.circular(8),
            onTap: () => openAlbum(context, player, a.id),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                FlexCover(path: a.coverMedium),
                const SizedBox(height: 8),
                Text(
                  a.title,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: const TextStyle(fontWeight: FontWeight.w600),
                ),
                Text(
                  [a.artist, if (a.year != null) '${a.year}'].where((s) => s.isNotEmpty).join(' · '),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: t.textTheme.bodySmall?.copyWith(color: t.colorScheme.onSurfaceVariant),
                ),
              ],
            ),
          );
        },
      ),
    );
  }
}

class AlbumPage extends StatelessWidget {
  const AlbumPage({super.key, required this.player, required this.albumId});

  final PlayerController player;
  final int albumId;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      backgroundColor: Colors.transparent,
      body: LibraryFuture<(AlbumInfo?, List<TrackInfo>)>(
        player: player,
        load: () async => (await album(albumId: albumId), await albumTracks(albumId: albumId)),
        builder: (context, data) {
          final (info, tracks) = data;
          if (info == null) return const EmptyState(icon: Icons.album_outlined, text: 'Album už v knihovně není');
          final details = [
            if (info.year != null) '${info.year}',
            tracksLabel(info.trackCount),
            formatLong(info.durationMs),
          ].join(' · ');
          return DetailScroll(
            cover: info.coverLarge ?? info.coverMedium,
            kind: 'Album',
            title: info.title,
            subtitle: [info.artist, details].where((s) => s.isNotEmpty).join(' · '),
            tracks: tracks,
            player: player,
            numbered: true,
            actions: [
              if (info.coverMedium == null) _FindCoverButton(player: player, albumId: info.id),
            ],
          );
        },
      ),
    );
  }
}

// ----- artists --------------------------------------------------------------------------

class ArtistsPage extends StatelessWidget {
  const ArtistsPage({super.key, required this.player, this.header});

  final PlayerController player;
  final Widget? header;

  @override
  Widget build(BuildContext context) {
    final t = Theme.of(context);
    return LibraryFuture<List<ArtistInfo>>(
      player: player,
      load: artists,
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
                    title: 'Interpreti',
                    subtitle: plural(list.length, 'interpret', 'interpreti', 'interpretů'),
                  ),
                ],
              ),
            ),
          ),
          SliverPadding(
            padding: const EdgeInsets.fromLTRB(16, 0, 16, 24),
            sliver: SliverGrid.builder(
              gridDelegate: const SliverGridDelegateWithMaxCrossAxisExtent(
                maxCrossAxisExtent: 170,
                mainAxisSpacing: 18,
                crossAxisSpacing: 18,
                childAspectRatio: .8,
              ),
              itemCount: list.length,
              itemBuilder: (context, i) {
                final a = list[i];
                return InkWell(
                  borderRadius: BorderRadius.circular(8),
                  onTap: () => openArtist(context, player, a),
                  child: Column(
                    children: [
                      Padding(
                        padding: const EdgeInsets.all(8),
                        child: FlexCover(path: a.coverMedium, circle: true),
                      ),
                      Text(
                        a.name,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: const TextStyle(fontWeight: FontWeight.w600),
                      ),
                      Text(
                        '${plural(a.albumCount, 'album', 'alba', 'alb')} · ${tracksLabel(a.trackCount)}',
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

class ArtistPage extends StatelessWidget {
  const ArtistPage({super.key, required this.player, required this.artist});

  final PlayerController player;
  final ArtistInfo artist;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      backgroundColor: Colors.transparent,
      body: LibraryFuture<(List<AlbumInfo>, List<TrackInfo>)>(
        player: player,
        load: () async => (await artistAlbums(artistId: artist.id), await artistTracks(artistId: artist.id)),
        builder: (context, data) {
          final (albumList, tracks) = data;
          return DetailScroll(
            cover: artist.coverMedium,
            circle: true,
            kind: 'Interpret',
            title: artist.name,
            subtitle: '${plural(albumList.length, 'album', 'alba', 'alb')} · ${tracksLabel(tracks.length)}',
            tracks: tracks,
            player: player,
            numbered: false,
            albums: albumList,
          );
        },
      ),
    );
  }
}

/// Header with a big cover and play buttons, followed by tracks (and albums).
class DetailScroll extends StatelessWidget {
  const DetailScroll({
    super.key,
    required this.cover,
    required this.kind,
    required this.title,
    required this.subtitle,
    required this.tracks,
    required this.player,
    required this.numbered,
    this.albums,
    this.circle = false,
    this.actions = const [],
  });

  final String? cover;
  final String kind;
  final String title;
  final String subtitle;
  final List<TrackInfo> tracks;
  final PlayerController player;
  final bool numbered;
  final List<AlbumInfo>? albums;
  final bool circle;

  /// Extra buttons after the play buttons.
  final List<Widget> actions;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final canPop = Navigator.of(context).canPop();
    return LayoutBuilder(
      builder: (context, c) {
        final wide = c.maxWidth >= 640;
        final coverSize = wide ? 180.0 : 170.0;
        final text = Column(
          crossAxisAlignment: wide ? CrossAxisAlignment.start : CrossAxisAlignment.center,
          children: [
            Text(
              kind.toUpperCase(),
              style: theme.textTheme.labelSmall?.copyWith(color: scheme.onSurfaceVariant, letterSpacing: .8),
            ),
            Text(
              title,
              textAlign: wide ? TextAlign.start : TextAlign.center,
              style: (wide ? theme.textTheme.displaySmall : theme.textTheme.headlineSmall)?.copyWith(
                fontWeight: FontWeight.w700,
              ),
            ),
            const SizedBox(height: 4),
            Text(
              subtitle,
              textAlign: wide ? TextAlign.start : TextAlign.center,
              style: TextStyle(color: scheme.onSurfaceVariant),
            ),
            const SizedBox(height: 14),
            Wrap(
              spacing: 8,
              runSpacing: 8,
              alignment: WrapAlignment.center,
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
                FilledButton.tonalIcon(
                  onPressed: tracks.isEmpty ? null : () => player.playNext(tracks),
                  icon: const Icon(Icons.queue_play_next),
                  label: const Text('Přehrát další'),
                ),
                IconButton(
                  tooltip: 'Přidat do playlistu',
                  onPressed: tracks.isEmpty ? null : () => showAddToPlaylist(context, player, tracks),
                  icon: const Icon(Icons.library_add_outlined),
                ),
                ...actions,
              ],
            ),
          ],
        );
        final coverWidget = CoverImage(path: cover, size: coverSize, radius: 10, circle: circle);
        return ListenableBuilder(
          listenable: player,
          builder: (context, _) => CustomScrollView(
            slivers: [
              SliverToBoxAdapter(
                child: Container(
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
                      if (canPop)
                        TextButton.icon(
                          onPressed: () => Navigator.of(context).pop(),
                          icon: const Icon(Icons.arrow_back),
                          label: const Text('Zpět'),
                        ),
                      const SizedBox(height: 8),
                      wide
                          ? Row(
                              crossAxisAlignment: CrossAxisAlignment.end,
                              children: [
                                coverWidget,
                                const SizedBox(width: 22),
                                Expanded(child: text),
                              ],
                            )
                          : Center(child: Column(children: [coverWidget, const SizedBox(height: 14), text])),
                    ],
                  ),
                ),
              ),
              if (albums != null && albums!.isNotEmpty) ...[
                SliverPadding(
                  padding: const EdgeInsets.fromLTRB(20, 8, 20, 8),
                  sliver: SliverToBoxAdapter(child: Text('Alba', style: theme.textTheme.titleMedium)),
                ),
                AlbumGrid(albums: albums!, player: player),
                SliverPadding(
                  padding: const EdgeInsets.fromLTRB(20, 0, 20, 8),
                  sliver: SliverToBoxAdapter(child: Text('Skladby', style: theme.textTheme.titleMedium)),
                ),
              ],
              SliverPadding(
                padding: const EdgeInsets.fromLTRB(12, 0, 12, 24),
                sliver: SliverList.builder(
                  itemCount: tracks.length,
                  itemBuilder: (context, i) => TrackRow(
                    track: tracks[i],
                    player: player,
                    number: numbered ? (tracks[i].trackNo ?? i + 1) : null,
                    showCover: !numbered,
                    showAlbum: !numbered,
                    // In a list of an album or artist, a tap plays the list from there.
                    onTap: () => player.playAll(tracks, start: i),
                    openAlbum: numbered ? null : (id) => openAlbum(context, player, id),
                  ),
                ),
              ),
            ],
          ),
        );
      },
    );
  }
}

/// Looks the album's cover up online (MusicBrainz, Cover Art Archive).
class _FindCoverButton extends StatefulWidget {
  const _FindCoverButton({required this.player, required this.albumId});

  final PlayerController player;
  final int albumId;

  @override
  State<_FindCoverButton> createState() => _FindCoverButtonState();
}

class _FindCoverButtonState extends State<_FindCoverButton> {
  bool _busy = false;

  Future<void> _find() async {
    setState(() => _busy = true);
    try {
      final found = await findAlbumCover(albumId: widget.albumId, force: true);
      widget.player.messages.add(found ? 'Obal nalezen' : 'Obal se na MusicBrainz nenašel');
      if (found) widget.player.libraryEdited();
    } catch (e) {
      widget.player.messages.add('Obal se nepodařilo načíst: $e');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return _busy
        ? const Padding(
            padding: EdgeInsets.all(12),
            child: SizedBox(width: 20, height: 20, child: CircularProgressIndicator(strokeWidth: 2)),
          )
        : IconButton(
            tooltip: 'Najít obal online (MusicBrainz)',
            onPressed: _find,
            icon: const Icon(Icons.image_search),
          );
  }
}
