import 'dart:async';

import 'package:flutter/material.dart';

import '../player_controller.dart';
import 'eq_page.dart';
import 'library_pages.dart';
import 'player.dart';
import 'queue.dart';
import 'settings_page.dart';
import 'theme.dart';
import 'widgets.dart';

enum Section { tracks, albums, artists, playlists, nowPlaying, eq, settings }

/// Width from which the desktop layout (side navigation + player bar) is used.
const desktopBreakpoint = 900.0;

/// Width from which the queue is a permanent column instead of a side drawer.
const queueColumnBreakpoint = 1150.0;

/// The app frame. Desktop: navigation, content, queue column and a player bar.
/// Mobile: content with bottom navigation, a mini player and a full-screen now playing.
class AppShell extends StatefulWidget {
  const AppShell({super.key, required this.player, required this.prefs});

  final PlayerController player;
  final UiPrefs prefs;

  @override
  State<AppShell> createState() => _AppShellState();
}

class _AppShellState extends State<AppShell> {
  Section _section = Section.tracks;
  int _tab = 0; // mobile: library, search, queue, settings
  final _navKeys = <Object, GlobalKey<NavigatorState>>{};
  StreamSubscription<String>? _messages;
  final _scaffold = GlobalKey<ScaffoldState>();

  PlayerController get player => widget.player;

  @override
  void initState() {
    super.initState();
    _messages = player.messages.stream.listen((m) {
      if (!mounted) return;
      ScaffoldMessenger.of(context)
        ..hideCurrentSnackBar()
        ..showSnackBar(SnackBar(content: Text(m), behavior: SnackBarBehavior.floating, width: 420));
    });
  }

  @override
  void dispose() {
    _messages?.cancel();
    super.dispose();
  }

  GlobalKey<NavigatorState> _key(Object id) => _navKeys.putIfAbsent(id, GlobalKey<NavigatorState>.new);

  /// A navigator for one section, so album and artist pages open inside the content area.
  Widget _contentNavigator(Object id, WidgetBuilder root) {
    final key = _key(id);
    return NavigatorPopHandler(
      onPopWithResult: (_) => key.currentState?.maybePop(),
      child: Navigator(
        key: key,
        onGenerateRoute: (_) => MaterialPageRoute(builder: root),
      ),
    );
  }

  void _select(Section s) {
    if (s == _section) {
      _key(s).currentState?.popUntil((r) => r.isFirst);
      return;
    }
    setState(() => _section = s);
  }

  Widget _desktopPage(Section s) => switch (s) {
    Section.tracks => TracksPage(player: player),
    Section.albums => AlbumsPage(player: player),
    Section.artists => ArtistsPage(player: player),
    Section.playlists => const EmptyState(icon: Icons.queue_music, text: 'Playlisty přijdou v další verzi.'),
    Section.nowPlaying => NowPlayingView(player: player),
    Section.eq => EqPage(player: player),
    Section.settings => SettingsPage(player: player, prefs: widget.prefs, onOpenEq: () => _select(Section.eq)),
  };

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(builder: (context, c) => c.maxWidth >= desktopBreakpoint ? _desktop(c.maxWidth) : _mobile());
  }

  // ----- desktop ------------------------------------------------------------------------

  Widget _desktop(double width) {
    final scheme = Theme.of(context).colorScheme;
    final queueColumn = width >= queueColumnBreakpoint;
    return Scaffold(
      key: _scaffold,
      endDrawer: queueColumn
          ? null
          : Drawer(
              width: 340,
              backgroundColor: scheme.surfaceContainer,
              child: SafeArea(child: QueueView(player: player)),
            ),
      body: Column(
        children: [
          Expanded(
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                _SideNav(
                  section: _section,
                  player: player,
                  onSelect: _select,
                  onOpenQueue: queueColumn ? null : () => _scaffold.currentState?.openEndDrawer(),
                ),
                Expanded(
                  child: KeyedSubtree(
                    key: ValueKey(_section),
                    child: _contentNavigator(_section, (_) => _desktopPage(_section)),
                  ),
                ),
                if (queueColumn)
                  Container(
                    width: 340,
                    decoration: BoxDecoration(
                      color: scheme.surfaceContainer,
                      border: Border(left: BorderSide(color: scheme.outlineVariant)),
                    ),
                    child: QueueView(player: player),
                  ),
              ],
            ),
          ),
          PlayerBar(
            player: player,
            onOpenNowPlaying: () => _select(Section.nowPlaying),
            onOpenEq: () => _select(Section.eq),
          ),
        ],
      ),
    );
  }

  // ----- mobile -------------------------------------------------------------------------

  void _openNowPlaying() {
    Navigator.of(context).push(
      PageRouteBuilder<void>(
        transitionDuration: const Duration(milliseconds: 280),
        reverseTransitionDuration: const Duration(milliseconds: 220),
        pageBuilder: (routeContext, _, _) => Scaffold(
          body: GestureDetector(
            onVerticalDragEnd: (d) {
              if ((d.primaryVelocity ?? 0) > 300) Navigator.pop(routeContext);
            },
            child: NowPlayingView(
              player: player,
              onClose: () => Navigator.pop(routeContext),
              onOpenQueue: () {
                Navigator.pop(routeContext);
                setState(() => _tab = 2);
              },
              onOpenEq: () => Navigator.of(routeContext).push(
                MaterialPageRoute(
                  builder: (_) => Scaffold(
                    body: SafeArea(child: EqPage(player: player, showBack: true)),
                  ),
                ),
              ),
            ),
          ),
        ),
        transitionsBuilder: (_, anim, _, child) => SlideTransition(
          position: Tween(
            begin: const Offset(0, 1),
            end: Offset.zero,
          ).animate(CurvedAnimation(parent: anim, curve: Curves.easeOutCubic)),
          child: child,
        ),
      ),
    );
  }

  Widget _mobile() {
    final pages = [
      _contentNavigator('m-library', (_) => _LibraryHome(player: player)),
      _contentNavigator('m-search', (_) => TracksPage(player: player, autofocusSearch: true)),
      QueueView(player: player),
      _contentNavigator(
        'm-settings',
        (navContext) => SettingsPage(
          player: player,
          prefs: widget.prefs,
          onOpenEq: () => Navigator.of(navContext).push(
            MaterialPageRoute(
              builder: (_) => Scaffold(body: EqPage(player: player, showBack: true)),
            ),
          ),
        ),
      ),
    ];
    return Scaffold(
      body: SafeArea(
        bottom: false,
        child: Column(
          children: [
            ListenableBuilder(
              listenable: player,
              builder: (_, _) => player.scanning
                  ? LinearProgressIndicator(
                      minHeight: 2,
                      value: player.scanTotal == 0 ? null : player.scanProcessed / player.scanTotal,
                    )
                  : const SizedBox(height: 2),
            ),
            Expanded(
              child: IndexedStack(index: _tab, children: pages),
            ),
          ],
        ),
      ),
      bottomNavigationBar: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          MiniPlayer(player: player, onOpen: _openNowPlaying),
          NavigationBar(
            selectedIndex: _tab,
            height: 64,
            labelBehavior: NavigationDestinationLabelBehavior.alwaysShow,
            onDestinationSelected: (i) {
              if (i == _tab) {
                final key = _navKeys[['m-library', 'm-search', '', 'm-settings'][i]];
                key?.currentState?.popUntil((r) => r.isFirst);
              }
              setState(() => _tab = i);
            },
            destinations: const [
              NavigationDestination(
                icon: Icon(Icons.library_music_outlined),
                selectedIcon: Icon(Icons.library_music),
                label: 'Knihovna',
              ),
              NavigationDestination(icon: Icon(Icons.search), label: 'Hledat'),
              NavigationDestination(
                icon: Icon(Icons.queue_music_outlined),
                selectedIcon: Icon(Icons.queue_music),
                label: 'Fronta',
              ),
              NavigationDestination(
                icon: Icon(Icons.settings_outlined),
                selectedIcon: Icon(Icons.settings),
                label: 'Nastavení',
              ),
            ],
          ),
        ],
      ),
    );
  }
}

/// Mobile library: tracks, albums or artists, switched by chips.
class _LibraryHome extends StatefulWidget {
  const _LibraryHome({required this.player});

  final PlayerController player;

  @override
  State<_LibraryHome> createState() => _LibraryHomeState();
}

class _LibraryHomeState extends State<_LibraryHome> {
  int _view = 0;

  @override
  Widget build(BuildContext context) {
    final header = Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: Wrap(
        spacing: 6,
        children: [
          for (final (i, label) in const ['Skladby', 'Alba', 'Interpreti'].indexed)
            ChoiceChip(label: Text(label), selected: _view == i, onSelected: (_) => setState(() => _view = i)),
        ],
      ),
    );
    return switch (_view) {
      1 => AlbumsPage(player: widget.player, header: header),
      2 => ArtistsPage(player: widget.player, header: header),
      _ => TracksPage(player: widget.player, header: header),
    };
  }
}

class _SideNav extends StatelessWidget {
  const _SideNav({required this.section, required this.player, required this.onSelect, this.onOpenQueue});

  final Section section;
  final PlayerController player;
  final ValueChanged<Section> onSelect;
  final VoidCallback? onOpenQueue;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    Widget item(Section s, IconData icon, String label) {
      final on = s == section || (s == Section.albums && section == Section.albums);
      return Padding(
        padding: const EdgeInsets.symmetric(vertical: 1),
        child: Material(
          color: on ? scheme.surfaceContainerHighest : Colors.transparent,
          borderRadius: BorderRadius.circular(8),
          child: InkWell(
            borderRadius: BorderRadius.circular(8),
            onTap: () => onSelect(s),
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 9),
              child: Row(
                children: [
                  Icon(icon, size: 19, color: on ? scheme.onSurface : scheme.onSurfaceVariant),
                  const SizedBox(width: 12),
                  Text(label, style: TextStyle(color: on ? scheme.onSurface : scheme.onSurfaceVariant)),
                ],
              ),
            ),
          ),
        ),
      );
    }

    Widget label(String text) => Padding(
      padding: const EdgeInsets.fromLTRB(10, 10, 10, 6),
      child: Text(text.toUpperCase(), style: TextStyle(fontSize: 11, letterSpacing: .8, color: scheme.outline)),
    );

    return Container(
      width: 216,
      decoration: BoxDecoration(
        color: scheme.surfaceContainer,
        border: Border(right: BorderSide(color: scheme.outlineVariant)),
      ),
      padding: const EdgeInsets.fromLTRB(10, 16, 10, 12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(10, 4, 10, 12),
            child: Row(
              children: [
                Container(
                  width: 22,
                  height: 22,
                  decoration: BoxDecoration(color: scheme.primary, borderRadius: BorderRadius.circular(6)),
                  child: Icon(Icons.graphic_eq, size: 15, color: scheme.onPrimary),
                ),
                const SizedBox(width: 8),
                const Text('DaveAudioPlayer', style: TextStyle(fontWeight: FontWeight.w700)),
              ],
            ),
          ),
          label('Knihovna'),
          item(Section.tracks, Icons.music_note_outlined, 'Skladby'),
          item(Section.albums, Icons.album_outlined, 'Alba'),
          item(Section.artists, Icons.person_outline, 'Interpreti'),
          item(Section.playlists, Icons.queue_music, 'Playlisty'),
          const Padding(padding: EdgeInsets.symmetric(vertical: 10, horizontal: 6), child: Divider()),
          item(Section.nowPlaying, Icons.play_circle_outline, 'Právě hraje'),
          item(Section.eq, Icons.equalizer, 'Ekvalizér'),
          item(Section.settings, Icons.settings_outlined, 'Nastavení'),
          if (onOpenQueue != null) ...[
            const SizedBox(height: 4),
            Padding(
              padding: const EdgeInsets.symmetric(vertical: 1),
              child: InkWell(
                borderRadius: BorderRadius.circular(8),
                onTap: onOpenQueue,
                child: Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 9),
                  child: Row(
                    children: [
                      Icon(Icons.queue_music_outlined, size: 19, color: scheme.onSurfaceVariant),
                      const SizedBox(width: 12),
                      Text('Fronta', style: TextStyle(color: scheme.onSurfaceVariant)),
                    ],
                  ),
                ),
              ),
            ),
          ],
          const Spacer(),
          ListenableBuilder(
            listenable: player,
            builder: (context, _) {
              if (!player.scanning) {
                if (player.analysisPending == 0 || player.analysisTotal == 0) return const SizedBox.shrink();
                final done = player.analysisTotal - player.analysisPending;
                return Tooltip(
                  message: 'Tónina a hlasitost se měří na pozadí',
                  child: Container(
                    padding: const EdgeInsets.all(10),
                    decoration: BoxDecoration(
                      color: scheme.surfaceContainerHigh,
                      borderRadius: BorderRadius.circular(8),
                    ),
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text('Analyzuji knihovnu…', style: TextStyle(fontSize: 12, color: scheme.onSurfaceVariant)),
                        Text('$done / ${player.analysisTotal}', style: const TextStyle(fontSize: 12)),
                        const SizedBox(height: 6),
                        LinearProgressIndicator(
                          value: done / player.analysisTotal,
                          minHeight: 4,
                          borderRadius: BorderRadius.circular(4),
                        ),
                      ],
                    ),
                  ),
                );
              }
              final folder = player.scanFolder?.split(RegExp(r'[\\/]')).where((p) => p.isNotEmpty).lastOrNull ?? '';
              return Container(
                padding: const EdgeInsets.all(10),
                decoration: BoxDecoration(color: scheme.surfaceContainerHigh, borderRadius: BorderRadius.circular(8)),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text('Hledám nové skladby…', style: TextStyle(fontSize: 12, color: scheme.onSurfaceVariant)),
                    Text(
                      '$folder ${player.scanProcessed} / ${player.scanTotal}',
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: const TextStyle(fontSize: 12),
                    ),
                    const SizedBox(height: 6),
                    LinearProgressIndicator(
                      value: player.scanTotal == 0 ? null : player.scanProcessed / player.scanTotal,
                      minHeight: 4,
                      borderRadius: BorderRadius.circular(4),
                    ),
                  ],
                ),
              );
            },
          ),
        ],
      ),
    );
  }
}
