import 'dart:io';

import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';

import '../platform.dart';
import '../player_controller.dart';
import '../src/rust/api/library.dart';
import '../src/rust/api/player.dart' as rust;
import 'widgets.dart';

/// Adds music folders without the user typing a path.
///
/// Windows: the system folder dialog. Android: the folders that contain music (found via
/// MediaStore) to tick, plus a folder browser for anything else.
Future<void> addMusicFolders(BuildContext context, PlayerController player) async {
  if (!Platform.isAndroid) {
    final dir = await getDirectoryPath(
      initialDirectory: await AppPlatform.defaultMusicDir(),
      confirmButtonText: 'Přidat složku',
    );
    if (dir != null) await rust.addFolder(path: dir);
    return;
  }
  if (!await AppPlatform.ensureAudioPermission()) {
    player.messages.add('Bez oprávnění ke čtení hudby nelze složku načíst');
    return;
  }
  final existing = [for (final f in await libraryFolders()) f.path];
  final found = await AppPlatform.musicFolders();
  if (!context.mounted) return;
  final chosen = await Navigator.of(context, rootNavigator: true).push<List<String>>(
    MaterialPageRoute(
      fullscreenDialog: true,
      builder: (_) => _MusicFoldersPage(found: found, existing: existing),
    ),
  );
  for (final path in chosen ?? const <String>[]) {
    await rust.addFolder(path: path);
  }
  if (chosen != null && chosen.isNotEmpty) {
    player.messages.add(chosen.length == 1 ? 'Složka přidána, načítám hudbu…' : 'Složky přidány, načítám hudbu…');
  }
}

/// "Internal storage" / "SD card" for volume roots, the folder name otherwise.
String folderLabel(String path) {
  if (path == '/storage/emulated/0') return 'Interní úložiště';
  final parts = path.split('/').where((p) => p.isNotEmpty).toList();
  if (parts.length == 2 && parts[0] == 'storage') return 'SD karta (${parts[1]})';
  return parts.isEmpty ? path : parts.last;
}

/// Shorter path for display: the internal storage prefix is left out.
String friendlyPath(String path) {
  const internal = '/storage/emulated/0';
  if (path == internal) return 'Interní úložiště';
  if (path.startsWith('$internal/')) return 'Interní úložiště/${path.substring(internal.length + 1)}';
  return path;
}

bool _covered(String path, List<String> roots) =>
    roots.any((r) => path == r || path.startsWith(r.endsWith('/') ? r : '$r/'));

class _MusicFoldersPage extends StatefulWidget {
  const _MusicFoldersPage({required this.found, required this.existing});

  final List<(String, int)> found;
  final List<String> existing;

  @override
  State<_MusicFoldersPage> createState() => _MusicFoldersPageState();
}

class _MusicFoldersPageState extends State<_MusicFoldersPage> {
  late final Set<String> _selected = {
    // Preselect everything with music that is not in the library yet.
    for (final (path, _) in widget.found)
      if (!_covered(path, widget.existing)) path,
  };
  final _extra = <String>[];

  Future<void> _browse() async {
    final path = await Navigator.of(context)
        .push<String>(MaterialPageRoute(builder: (_) => FolderBrowserPage(roots: AppPlatform.storageRoots())));
    if (path == null || !mounted) return;
    setState(() {
      if (!_extra.contains(path) && !widget.found.any((f) => f.$1 == path)) _extra.add(path);
      _selected.add(path);
    });
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final rows = [
      for (final (path, count) in widget.found) (path, tracksLabel(count)),
      for (final path in _extra) (path, 'vybráno ručně'),
    ];
    return Scaffold(
      appBar: AppBar(title: const Text('Přidat hudbu')),
      body: ListView(
        padding: const EdgeInsets.only(bottom: 24),
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 8, 16, 8),
            child: Text(
              widget.found.isEmpty
                  ? 'V telefonu jsme nenašli žádnou hudbu. Vyber složku ručně.'
                  : 'Tady je v telefonu hudba. Vyber složky, které chceš mít v knihovně:',
              style: TextStyle(color: scheme.onSurfaceVariant),
            ),
          ),
          for (final (path, detail) in rows)
            Builder(
              builder: (context) {
                final inLibrary = _covered(path, widget.existing);
                return CheckboxListTile(
                  value: inLibrary || _selected.contains(path),
                  onChanged: inLibrary
                      ? null
                      : (v) => setState(() => v == true ? _selected.add(path) : _selected.remove(path)),
                  secondary: Icon(Icons.folder_outlined, color: scheme.onSurfaceVariant),
                  title: Text(folderLabel(path)),
                  subtitle: Text(inLibrary ? '$detail · už je v knihovně' : '$detail · ${friendlyPath(path)}'),
                );
              },
            ),
          const SizedBox(height: 8),
          ListTile(
            leading: Icon(Icons.travel_explore, color: scheme.primary),
            title: const Text('Vybrat jinou složku…'),
            subtitle: const Text('Procházet úložiště telefonu a SD kartu'),
            onTap: _browse,
          ),
        ],
      ),
      bottomNavigationBar: SafeArea(
        child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 8, 16, 12),
          child: FilledButton(
            onPressed: _selected.isEmpty ? null : () => Navigator.pop(context, _selected.toList()),
            child: Text(_selected.isEmpty ? 'Vyber složku' : 'Přidat (${_selected.length})'),
          ),
        ),
      ),
    );
  }
}

/// A simple folder browser: storage volumes, then their subfolders. Returns the chosen path.
class FolderBrowserPage extends StatefulWidget {
  const FolderBrowserPage({super.key, required this.roots});

  final List<String> roots;

  @override
  State<FolderBrowserPage> createState() => _FolderBrowserPageState();
}

class _FolderBrowserPageState extends State<FolderBrowserPage> {
  String? _dir; // null = list of volumes
  List<Directory> _children = const [];
  bool _loading = false;

  @override
  void initState() {
    super.initState();
    if (widget.roots.length == 1) _open(widget.roots.single);
  }

  Future<void> _open(String? dir) async {
    setState(() {
      _dir = dir;
      _loading = dir != null;
      _children = const [];
    });
    if (dir == null) return;
    final children = <Directory>[];
    try {
      await for (final e in Directory(dir).list(followLinks: false)) {
        final name = e.path.split('/').last;
        if (e is Directory && !name.startsWith('.')) children.add(e);
      }
    } catch (_) {
      // Unreadable: shown as empty.
    }
    children.sort((a, b) => a.path.toLowerCase().compareTo(b.path.toLowerCase()));
    if (!mounted || _dir != dir) return;
    setState(() {
      _children = children;
      _loading = false;
    });
  }

  void _up() {
    final dir = _dir;
    if (dir == null) {
      Navigator.pop(context);
      return;
    }
    if (widget.roots.contains(dir)) {
      widget.roots.length == 1 ? Navigator.pop(context) : _open(null);
      return;
    }
    _open(dir.substring(0, dir.lastIndexOf('/')));
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final dir = _dir;
    return PopScope(
      canPop: false,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop) _up();
      },
      child: Scaffold(
        appBar: AppBar(
          leading: IconButton(icon: const Icon(Icons.arrow_back), onPressed: _up),
          title: Text(dir == null ? 'Úložiště' : folderLabel(dir)),
          bottom: dir == null
              ? null
              : PreferredSize(
                  preferredSize: const Size.fromHeight(24),
                  child: Padding(
                    padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
                    child: Align(
                      alignment: Alignment.centerLeft,
                      child: Text(
                        friendlyPath(dir),
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: TextStyle(fontSize: 12.5, color: scheme.onSurfaceVariant),
                      ),
                    ),
                  ),
                ),
        ),
        body: dir == null
            ? ListView(
                children: [
                  for (final r in widget.roots)
                    ListTile(
                      leading: Icon(r.contains('emulated') ? Icons.phone_android : Icons.sd_card_outlined),
                      title: Text(folderLabel(r)),
                      onTap: () => _open(r),
                    ),
                ],
              )
            : _loading
            ? const Center(child: CircularProgressIndicator())
            : _children.isEmpty
            ? const EmptyState(icon: Icons.folder_open, text: 'Žádné podsložky.\nTuto složku můžeš vybrat.')
            : ListView.builder(
                itemCount: _children.length,
                itemBuilder: (context, i) {
                  final d = _children[i];
                  return ListTile(
                    leading: Icon(Icons.folder_outlined, color: scheme.onSurfaceVariant),
                    title: Text(d.path.split('/').last),
                    trailing: const Icon(Icons.chevron_right),
                    onTap: () => _open(d.path),
                  );
                },
              ),
        bottomNavigationBar: dir == null
            ? null
            : SafeArea(
                child: Padding(
                  padding: const EdgeInsets.fromLTRB(16, 8, 16, 12),
                  child: FilledButton.icon(
                    onPressed: () => Navigator.pop(context, dir),
                    icon: const Icon(Icons.check),
                    label: Text('Vybrat „${folderLabel(dir)}“'),
                  ),
                ),
              ),
      ),
    );
  }
}
