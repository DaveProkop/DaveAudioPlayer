import 'dart:io';

import 'package:flutter/material.dart';

import '../player_controller.dart';
import '../src/rust/api/library.dart';
import '../src/rust/api/player.dart' as rust;
import 'folder_picker.dart';
import 'library_pages.dart' show LibraryFuture;
import 'player.dart' show mixModeDescription, mixModeName;
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
                      title: const Text('Režim mixu'),
                      subtitle: Text(s == null ? '' : mixModeDescription(s.mixMode)),
                    ),
                    Padding(
                      padding: const EdgeInsets.fromLTRB(16, 0, 16, 12),
                      child: SegmentedButton<rust.MixModeDto>(
                        segments: [
                          for (final m in rust.MixModeDto.values)
                            ButtonSegment(value: m, label: Text(mixModeName(m))),
                        ],
                        selected: {s?.mixMode ?? rust.MixModeDto.party},
                        showSelectedIcon: false,
                        onSelectionChanged: s == null ? null : (v) => rust.setMixMode(mode: v.first),
                      ),
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
                    SwitchListTile(
                      title: const Text('Hledat texty písní online'),
                      subtitle: const Text('Když skladba nemá text v tagu ani v souboru .lrc, zkusí se LRCLIB (lrclib.net).'),
                      value: prefs.lyricsOnline,
                      onChanged: prefs.setLyricsOnline,
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
                    const _OutputDeviceTile(),
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

/// The sound device to play on; tapping lists the devices.
class _OutputDeviceTile extends StatefulWidget {
  const _OutputDeviceTile();

  @override
  State<_OutputDeviceTile> createState() => _OutputDeviceTileState();
}

class _OutputDeviceTileState extends State<_OutputDeviceTile> {
  late Future<rust.OutputDevices> _devices = rust.outputDevices();

  Future<void> _choose(rust.OutputDevices devices) async {
    // '' stands for the system default.
    final chosen = await showDialog<String>(
      context: context,
      builder: (dialog) => SimpleDialog(
        title: const Text('Výstupní zařízení'),
        children: [
          for (final (value, label) in [('', 'Výchozí zařízení systému'), for (final n in devices.names) (n, n)])
            SimpleDialogOption(
              onPressed: () => Navigator.of(dialog).pop(value),
              child: Row(
                children: [
                  Icon(
                    (devices.chosen ?? '') == value ? Icons.radio_button_checked : Icons.radio_button_unchecked,
                    size: 20,
                  ),
                  const SizedBox(width: 12),
                  Expanded(child: Text(label)),
                ],
              ),
            ),
        ],
      ),
    );
    if (chosen == null || chosen == (devices.chosen ?? '')) return;
    await rust.setOutputDevice(name: chosen.isEmpty ? null : chosen);
    setState(() {
      _devices = rust.outputDevices();
    });
  }

  @override
  Widget build(BuildContext context) {
    return FutureBuilder<rust.OutputDevices>(
      future: _devices,
      builder: (context, snap) {
        final d = snap.data;
        final chosen = d?.chosen;
        final missing = chosen != null && !d!.names.contains(chosen);
        return ListTile(
          title: const Text('Výstupní zařízení'),
          subtitle: Text(
            chosen == null
                ? 'Výchozí zařízení systému'
                : missing
                ? '$chosen (není připojené – hraje výchozí)'
                : chosen,
          ),
          trailing: const Icon(Icons.chevron_right),
          onTap: d == null ? null : () => _choose(d),
        );
      },
    );
  }
}
