import 'dart:io';

import 'package:flutter/material.dart';

import '../player_controller.dart';
import '../src/rust/api/library.dart';
import '../src/rust/api/player.dart' as rust;
import 'folder_picker.dart';
import 'library_pages.dart' show LibraryFuture;
import 'theme.dart';
import 'widgets.dart';

class SettingsPage extends StatelessWidget {
  const SettingsPage({super.key, required this.player, required this.prefs, this.onOpenEq});

  final PlayerController player;
  final UiPrefs prefs;
  final VoidCallback? onOpenEq;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return ListenableBuilder(
      listenable: Listenable.merge([player, prefs]),
      builder: (context, _) {
        final s = player.settings;
        return Align(
          alignment: Alignment.topLeft,
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 780),
            child: ListView(
              padding: const EdgeInsets.fromLTRB(16, 12, 16, 24),
              children: [
                const PageHeader(title: 'Nastavení', subtitle: 'Změny se uloží hned.'),
                _Card(
                  title: 'Složky s hudbou',
                  children: [
                    LibraryFuture<List<FolderInfo>>(
                      player: player,
                      load: libraryFolders,
                      builder: (context, folders) => Column(
                        children: [
                          if (folders.isEmpty)
                            const ListTile(
                              title: Text('Zatím žádná složka'),
                              subtitle: Text('Přidej složku, kde máš hudbu.'),
                            ),
                          for (final f in folders)
                            ListTile(
                              leading: Icon(
                                Icons.folder_outlined,
                                color: f.available ? scheme.onSurfaceVariant : context.appColors.warn,
                              ),
                              title: Text(friendlyPath(f.path)),
                              subtitle: Text(
                                f.available
                                    ? '${tracksLabel(f.trackCount)} · sleduje se'
                                    : '${tracksLabel(f.trackCount)} · složka není dostupná',
                                style: f.available ? null : TextStyle(color: context.appColors.warn),
                              ),
                              trailing: IconButton(
                                tooltip: 'Odebrat z knihovny',
                                icon: const Icon(Icons.remove_circle_outline),
                                onPressed: () => _confirmRemove(context, f),
                              ),
                            ),
                        ],
                      ),
                    ),
                    Padding(
                      padding: const EdgeInsets.fromLTRB(16, 4, 16, 12),
                      child: Wrap(
                        spacing: 8,
                        runSpacing: 8,
                        children: [
                          FilledButton.icon(
                            onPressed: () => addMusicFolders(context, player),
                            icon: const Icon(Icons.create_new_folder_outlined),
                            label: const Text('Přidat složku'),
                          ),
                          FilledButton.tonalIcon(
                            onPressed: player.scanning ? () => rust.cancelScan() : () => rust.rescanLibrary(),
                            icon: Icon(player.scanning ? Icons.stop : Icons.refresh),
                            label: Text(player.scanning ? 'Zastavit skenování' : 'Znovu načíst vše'),
                          ),
                        ],
                      ),
                    ),
                  ],
                ),
                _Card(
                  title: 'Přehrávání',
                  children: [
                    ListTile(
                      title: const Text('Crossfade'),
                      subtitle: const Text('Prolnutí na konci skladby. Navazující skladby alba hrají vždy bez mezery.'),
                    ),
                    Padding(
                      padding: const EdgeInsets.fromLTRB(8, 0, 16, 8),
                      child: Row(
                        children: [
                          Expanded(
                            child: Slider(
                              value: ((s?.crossfadeMs ?? 0) / 1000).clamp(0, 12).toDouble(),
                              max: 12,
                              divisions: 12,
                              label: _xf(s?.crossfadeMs ?? 0),
                              onChanged: s == null ? null : (v) => rust.setCrossfade(ms: v.round() * 1000),
                            ),
                          ),
                          SizedBox(width: 72, child: Text(_xf(s?.crossfadeMs ?? 0), textAlign: TextAlign.right)),
                        ],
                      ),
                    ),
                    SwitchListTile(
                      title: const Text('Beatmatch'),
                      subtitle: const Text(
                        'Skladby s podobným tempem se srovnají do rytmu: nová nastoupí na dobu, '
                        'basy se prohodí na hranici taktu a tempo se pak plynule vrátí. Ostatní se prolnou.',
                      ),
                      value: s?.beatmatch ?? true,
                      onChanged: s == null || (s.crossfadeMs == 0) ? null : (v) => rust.setBeatmatch(enabled: v),
                    ),
                    LibraryFuture<LibraryStats>(
                      player: player,
                      load: libraryStats,
                      builder: (context, stats) => SwitchListTile(
                        title: const Text('Vyrovnat hlasitost'),
                        subtitle: Text(
                          'EBU R128, −18 LUFS. '
                          '${stats.pendingAnalysis == 0 ? 'Všechny skladby jsou změřené.' : 'Na pozadí zbývá změřit ${tracksLabel(stats.pendingAnalysis)}.'}',
                        ),
                        value: s?.normalize ?? true,
                        onChanged: s == null ? null : (v) => rust.setNormalization(enabled: v),
                      ),
                    ),
                    if (Platform.isAndroid)
                      SwitchListTile(
                        title: const Text('Analyzovat celou knihovnu při nabíjení'),
                        subtitle: const Text(
                          'Tónina a hlasitost pro všechny skladby. Bez nabíjení se měří jen skladby ve frontě.',
                        ),
                        value: prefs.analyzeOnCharge,
                        onChanged: prefs.setAnalyzeOnCharge,
                      ),
                    ListTile(
                      title: const Text('Ekvalizér'),
                      subtitle: Text((s?.eqEnabled ?? false) ? 'Zapnutý' : 'Vypnutý'),
                      trailing: const Icon(Icons.chevron_right),
                      onTap: onOpenEq,
                    ),
                    const ListTile(title: Text('Výstupní zařízení'), subtitle: Text('Výchozí zařízení systému')),
                  ],
                ),
                _Card(
                  title: 'Vzhled',
                  children: [
                    ListTile(
                      title: const Text('Motiv'),
                      trailing: SegmentedButton<ThemeMode>(
                        showSelectedIcon: false,
                        segments: const [
                          ButtonSegment(value: ThemeMode.dark, label: Text('Tmavý')),
                          ButtonSegment(value: ThemeMode.light, label: Text('Světlý')),
                          ButtonSegment(value: ThemeMode.system, label: Text('Systém')),
                        ],
                        selected: {prefs.themeMode},
                        onSelectionChanged: (v) => prefs.setThemeMode(v.first),
                      ),
                    ),
                    SwitchListTile(
                      title: const Text('Barva z obalu'),
                      subtitle: const Text('Akcentová barva podle obalu právě hrané skladby'),
                      value: prefs.coverAccent,
                      onChanged: prefs.setCoverAccent,
                    ),
                  ],
                ),
                const _Card(
                  title: 'O aplikaci',
                  children: [
                    ListTile(
                      title: Text('DaveAudioPlayer 0.1'),
                      subtitle: Text('Přehrávač lokální hudby pro Windows a Android'),
                    ),
                  ],
                ),
              ],
            ),
          ),
        );
      },
    );
  }

  static String _xf(int ms) => ms == 0 ? 'vypnuto' : '${(ms / 1000).round()} s';
}

class _Card extends StatelessWidget {
  const _Card({required this.title, required this.children});

  final String title;
  final List<Widget> children;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    // A Material, not a coloured box: list tiles paint their ink on it.
    return Padding(
      padding: const EdgeInsets.only(bottom: 14),
      child: Material(
        color: scheme.surfaceContainer,
        borderRadius: BorderRadius.circular(12),
        clipBehavior: Clip.antiAlias,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 14, 16, 4),
              child: Text(
                title.toUpperCase(),
                style: TextStyle(fontSize: 12, letterSpacing: .8, fontWeight: FontWeight.w600, color: scheme.outline),
              ),
            ),
            ...children,
          ],
        ),
      ),
    );
  }
}

Future<void> _confirmRemove(BuildContext context, FolderInfo f) async {
  final ok = await showDialog<bool>(
    context: context,
    builder: (context) => AlertDialog(
      title: const Text('Odebrat složku z knihovny?'),
      content: Text(
        '${friendlyPath(f.path)}\n\n'
        '${tracksLabel(f.trackCount)} zmizí z knihovny, fronty i statistik. '
        'Soubory na disku zůstanou.',
      ),
      actions: [
        TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('Zrušit')),
        FilledButton(onPressed: () => Navigator.pop(context, true), child: const Text('Odebrat')),
      ],
    ),
  );
  if (ok == true) await rust.removeFolder(folderId: f.id);
}
