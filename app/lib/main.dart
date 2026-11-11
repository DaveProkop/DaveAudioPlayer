import 'dart:async';

import 'package:flutter_rust_bridge/flutter_rust_bridge.dart' show Int64List;
import 'package:flutter/material.dart';

import 'media_session.dart';
import 'platform.dart';
import 'player_controller.dart';
import 'src/rust/api/library.dart';
import 'src/rust/api/player.dart' as player;
import 'src/rust/frb_generated.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();
  final controller = PlayerController();
  String? startupError;
  try {
    final dir = await AppPlatform.dataDir();
    await controller.start('$dir/library.db');
    await MediaSession.start(controller);
  } catch (e) {
    startupError = e.toString();
  }
  runApp(DaveAudioPlayerApp(controller: controller, startupError: startupError));
}

class DaveAudioPlayerApp extends StatelessWidget {
  const DaveAudioPlayerApp({super.key, required this.controller, this.startupError});

  final PlayerController controller;
  final String? startupError;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'DaveAudioPlayer',
      theme: ThemeData(colorSchemeSeed: Colors.teal, useMaterial3: true),
      darkTheme: ThemeData(
        colorSchemeSeed: Colors.teal,
        brightness: Brightness.dark,
        useMaterial3: true,
      ),
      home: startupError != null
          ? Scaffold(body: Center(child: Text('Nelze spustit přehrávač:\n$startupError')))
          : HomeScreen(controller: controller),
    );
  }
}

class HomeScreen extends StatefulWidget {
  const HomeScreen({super.key, required this.controller});

  final PlayerController controller;

  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> {
  StreamSubscription<String>? _messages;

  @override
  void initState() {
    super.initState();
    _messages = widget.controller.messages.stream.listen((m) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(m)));
    });
  }

  @override
  void dispose() {
    _messages?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = widget.controller;
    return LayoutBuilder(builder: (context, constraints) {
      final wide = constraints.maxWidth >= 900;
      final library = LibraryPanel(controller: c);
      final queue = QueuePanel(controller: c);
      return Scaffold(
        appBar: AppBar(
          title: const Text('DaveAudioPlayer'),
          actions: [
            IconButton(
              tooltip: 'Přidat složku s hudbou',
              icon: const Icon(Icons.create_new_folder_outlined),
              onPressed: () => _addFolder(context),
            ),
            IconButton(
              tooltip: 'Znovu načíst knihovnu',
              icon: const Icon(Icons.refresh),
              onPressed: () => player.rescanLibrary(),
            ),
          ],
          bottom: PreferredSize(
            preferredSize: const Size.fromHeight(4),
            child: ListenableBuilder(
              listenable: c,
              builder: (_, _) => c.scanning
                  ? LinearProgressIndicator(
                      value: c.scanTotal == 0 ? null : c.scanProcessed / c.scanTotal,
                    )
                  : const SizedBox(height: 4),
            ),
          ),
        ),
        body: wide
            ? Row(children: [
                Expanded(flex: 3, child: library),
                const VerticalDivider(width: 1),
                Expanded(flex: 2, child: queue),
              ])
            : DefaultTabController(
                length: 2,
                child: Column(children: [
                  const TabBar(tabs: [Tab(text: 'Knihovna'), Tab(text: 'Fronta')]),
                  Expanded(child: TabBarView(children: [library, queue])),
                ]),
              ),
        bottomNavigationBar: PlayerBar(controller: c),
      );
    });
  }

  Future<void> _addFolder(BuildContext context) async {
    if (!await AppPlatform.ensureAudioPermission()) {
      widget.controller.messages.add('Bez oprávnění ke čtení hudby nelze složku načíst');
      return;
    }
    final text = TextEditingController(text: await AppPlatform.defaultMusicDir());
    if (!context.mounted) return;
    final path = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Přidat složku s hudbou'),
        content: TextField(
          controller: text,
          autofocus: true,
          decoration: const InputDecoration(labelText: 'Cesta ke složce'),
          onSubmitted: (v) => Navigator.pop(context, v),
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context), child: const Text('Zrušit')),
          FilledButton(onPressed: () => Navigator.pop(context, text.text), child: const Text('Přidat')),
        ],
      ),
    );
    if (path != null && path.trim().isNotEmpty) {
      await player.addFolder(path: path.trim());
    }
  }
}

// ----- library ------------------------------------------------------------------------

class LibraryPanel extends StatefulWidget {
  const LibraryPanel({super.key, required this.controller});

  final PlayerController controller;

  @override
  State<LibraryPanel> createState() => _LibraryPanelState();
}

class _LibraryPanelState extends State<LibraryPanel> {
  static const _pageSize = 100;

  final _search = TextEditingController();
  Timer? _debounce;
  int _count = 0;
  final Map<int, List<TrackInfo>> _pages = {};
  final Set<int> _loading = {};
  List<TrackInfo>? _results;
  int _seenVersion = -1;

  @override
  void initState() {
    super.initState();
    widget.controller.addListener(_onController);
    _reload();
  }

  @override
  void dispose() {
    widget.controller.removeListener(_onController);
    _debounce?.cancel();
    _search.dispose();
    super.dispose();
  }

  void _onController() {
    if (widget.controller.libraryVersion != _seenVersion) _reload();
  }

  Future<void> _reload() async {
    _seenVersion = widget.controller.libraryVersion;
    final count = await trackCount();
    if (!mounted) return;
    setState(() {
      _count = count;
      _pages.clear();
    });
    if (_search.text.isNotEmpty) _runSearch(_search.text);
  }

  void _onSearchChanged(String q) {
    _debounce?.cancel();
    _debounce = Timer(const Duration(milliseconds: 150), () => _runSearch(q));
  }

  Future<void> _runSearch(String q) async {
    if (q.trim().isEmpty) {
      setState(() => _results = null);
      return;
    }
    final r = await search(query: q, limit: 300);
    if (mounted && _search.text == q) setState(() => _results = r);
  }

  TrackInfo? _trackAt(int index) {
    final page = index ~/ _pageSize;
    final loaded = _pages[page];
    if (loaded != null) return index % _pageSize < loaded.length ? loaded[index % _pageSize] : null;
    if (_loading.add(page)) {
      libraryPage(offset: page * _pageSize, limit: _pageSize).then((tracks) {
        _loading.remove(page);
        if (mounted) setState(() => _pages[page] = tracks);
      });
    }
    return null;
  }

  @override
  Widget build(BuildContext context) {
    final results = _results;
    return Column(children: [
      Padding(
        padding: const EdgeInsets.all(8),
        child: TextField(
          controller: _search,
          onChanged: _onSearchChanged,
          decoration: InputDecoration(
            prefixIcon: const Icon(Icons.search),
            hintText: 'Hledat interpreta, skladbu, album…',
            border: const OutlineInputBorder(),
            isDense: true,
            suffixIcon: _search.text.isEmpty
                ? null
                : IconButton(
                    icon: const Icon(Icons.clear),
                    onPressed: () {
                      _search.clear();
                      _runSearch('');
                    },
                  ),
          ),
        ),
      ),
      Expanded(
        child: (results == null && _count == 0)
            ? const Center(child: Text('Knihovna je prázdná.\nPřidejte složku s hudbou (ikona nahoře).',
                textAlign: TextAlign.center))
            : ListView.builder(
                itemExtent: 56,
                itemCount: results?.length ?? _count,
                itemBuilder: (context, i) {
                  final t = results != null ? results[i] : _trackAt(i);
                  if (t == null) return const ListTile(title: Text('…'));
                  return TrackTile(
                    track: t,
                    onTap: () => widget.controller.addAndMaybePlay(t),
                    trailing: IconButton(
                      tooltip: 'Přidat do fronty',
                      icon: const Icon(Icons.playlist_add),
                      onPressed: () {
                        widget.controller.remember([t]);
                        player.enqueue(trackIds: Int64List.fromList([t.id]));
                      },
                    ),
                  );
                },
              ),
      ),
    ]);
  }
}

String formatMs(int? ms) {
  if (ms == null) return '–:––';
  final s = ms ~/ 1000;
  return '${s ~/ 60}:${(s % 60).toString().padLeft(2, '0')}';
}

class TrackTile extends StatelessWidget {
  const TrackTile({super.key, required this.track, this.onTap, this.trailing, this.selected = false});

  final TrackInfo track;
  final VoidCallback? onTap;
  final Widget? trailing;
  final bool selected;

  @override
  Widget build(BuildContext context) {
    final subtitle = [track.artist, track.album].where((s) => s.isNotEmpty).join(' · ');
    return ListTile(
      dense: true,
      selected: selected,
      enabled: !track.missing,
      title: Text(track.title, maxLines: 1, overflow: TextOverflow.ellipsis),
      subtitle: Text(subtitle, maxLines: 1, overflow: TextOverflow.ellipsis),
      leading: SizedBox(
        width: 40,
        child: Center(child: Text(formatMs(track.durationMs), style: Theme.of(context).textTheme.bodySmall)),
      ),
      trailing: trailing,
      onTap: onTap,
    );
  }
}

// ----- queue --------------------------------------------------------------------------

class QueuePanel extends StatelessWidget {
  const QueuePanel({super.key, required this.controller});

  final PlayerController controller;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: controller,
      builder: (context, _) {
        final queue = controller.queue;
        return Column(children: [
          ListTile(
            title: Text('Fronta (${queue.length})', style: Theme.of(context).textTheme.titleMedium),
            trailing: IconButton(
              tooltip: 'Vyprázdnit frontu',
              icon: const Icon(Icons.clear_all),
              onPressed: queue.isEmpty ? null : () => player.clearQueue(),
            ),
          ),
          Expanded(
            child: queue.isEmpty
                ? const Center(child: Text('Fronta je prázdná'))
                : ReorderableListView.builder(
                    buildDefaultDragHandles: false,
                    itemCount: queue.length,
                    // onReorderItem already reports the final index, as the engine expects.
                    onReorderItem: (from, to) => player.moveQueueItem(from: from, to: to),
                    itemBuilder: (context, i) {
                      final item = queue[i];
                      final t = controller.track(item.trackId);
                      final tile = t == null
                          ? ListTile(dense: true, title: Text('Skladba ${item.trackId}'))
                          : TrackTile(
                              track: t,
                              selected: controller.currentIndex == i,
                              onTap: () => player.playIndex(index: i),
                              trailing: ReorderableDragStartListener(
                                index: i,
                                child: const Icon(Icons.drag_handle),
                              ),
                            );
                      return Dismissible(
                        key: ValueKey(item.id),
                        direction: DismissDirection.endToStart,
                        background: Container(
                          color: Theme.of(context).colorScheme.errorContainer,
                          alignment: Alignment.centerRight,
                          padding: const EdgeInsets.only(right: 16),
                          child: const Icon(Icons.delete_outline),
                        ),
                        onDismissed: (_) => player.removeQueueItem(index: i),
                        child: tile,
                      );
                    },
                  ),
          ),
        ]);
      },
    );
  }
}

// ----- player bar ---------------------------------------------------------------------

class PlayerBar extends StatefulWidget {
  const PlayerBar({super.key, required this.controller});

  final PlayerController controller;

  @override
  State<PlayerBar> createState() => _PlayerBarState();
}

class _PlayerBarState extends State<PlayerBar> {
  Timer? _tick;
  double? _dragMs;

  @override
  void initState() {
    super.initState();
    // Repaint the position locally; no position traffic over the bridge.
    _tick = Timer.periodic(const Duration(milliseconds: 250), (_) {
      if (mounted && widget.controller.isPlaying && _dragMs == null) setState(() {});
    });
  }

  @override
  void dispose() {
    _tick?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = widget.controller;
    return ListenableBuilder(
      listenable: c,
      builder: (context, _) {
        final t = c.currentTrack;
        final duration = c.state?.durationMs ?? 0;
        final pos = _dragMs ?? c.positionMs.toDouble();
        return Material(
          elevation: 8,
          child: SafeArea(
            top: false,
            child: Padding(
              padding: const EdgeInsets.fromLTRB(16, 4, 16, 8),
              child: Column(mainAxisSize: MainAxisSize.min, children: [
                Row(children: [
                  Text(formatMs(pos.round())),
                  Expanded(
                    child: Slider(
                      value: duration > 0 ? pos.clamp(0, duration.toDouble()) : 0,
                      max: duration > 0 ? duration.toDouble() : 1,
                      onChanged: duration > 0 ? (v) => setState(() => _dragMs = v) : null,
                      onChangeEnd: (v) {
                        setState(() => _dragMs = null);
                        player.seek(positionMs: v.round());
                      },
                    ),
                  ),
                  Text(formatMs(duration > 0 ? duration : null)),
                ]),
                Row(children: [
                  Expanded(
                    child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
                      Text(t?.title ?? 'Nic nehraje',
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: Theme.of(context).textTheme.titleSmall),
                      Text(t == null ? '' : [t.artist, t.album].where((s) => s.isNotEmpty).join(' · '),
                          maxLines: 1, overflow: TextOverflow.ellipsis),
                    ]),
                  ),
                  IconButton(
                    tooltip: 'Předchozí',
                    icon: const Icon(Icons.skip_previous),
                    onPressed: () => player.previous(),
                  ),
                  IconButton.filled(
                    tooltip: c.isPlaying ? 'Pauza' : 'Přehrát',
                    iconSize: 32,
                    icon: Icon(c.isPlaying ? Icons.pause : Icons.play_arrow),
                    onPressed: () => player.togglePlayPause(),
                  ),
                  IconButton(
                    tooltip: 'Další',
                    icon: const Icon(Icons.skip_next),
                    onPressed: () => player.next(),
                  ),
                  SizedBox(
                    width: 140,
                    child: Row(children: [
                      const Icon(Icons.volume_up, size: 20),
                      Expanded(
                        child: Slider(
                          value: widget.controller.volume,
                          onChanged: (v) => widget.controller.setVolume(v),
                        ),
                      ),
                    ]),
                  ),
                ]),
              ]),
            ),
          ),
        );
      },
    );
  }
}
