import 'package:flutter/material.dart';

import '../player_controller.dart';
import '../src/rust/api/library.dart';
import '../src/rust/api/metadata.dart';

/// Edits a track's facts. Changed fields are locked against rescans; optionally they are
/// written into the file's tags too.
Future<void> showEditTrack(BuildContext context, PlayerController player, TrackInfo track) async {
  final current = await trackFields(trackId: track.id);
  if (current == null || !context.mounted) return;
  final saved = await showDialog<(TrackFieldsDto, bool)>(
    context: context,
    builder: (_) => _EditTrackDialog(fields: current),
  );
  if (saved == null) return;
  final error = await editTrack(trackId: track.id, fields: saved.$1, writeToFile: saved.$2);
  await player.refreshTrack(track.id);
  player.libraryEdited();
  player.messages.add(error == null ? 'Údaje uloženy' : 'Uloženo v knihovně, do souboru ne: $error');
}

class _EditTrackDialog extends StatefulWidget {
  const _EditTrackDialog({required this.fields});

  final TrackFieldsDto fields;

  @override
  State<_EditTrackDialog> createState() => _EditTrackDialogState();
}

class _EditTrackDialogState extends State<_EditTrackDialog> {
  late final _title = TextEditingController(text: widget.fields.title);
  late final _artists = TextEditingController(text: widget.fields.artists.join('; '));
  late final _album = TextEditingController(text: widget.fields.album);
  late final _albumArtist = TextEditingController(text: widget.fields.albumArtist);
  late final _year = TextEditingController(text: widget.fields.year?.toString() ?? '');
  late final _trackNo = TextEditingController(text: widget.fields.trackNo?.toString() ?? '');
  late final _discNo = TextEditingController(text: widget.fields.discNo?.toString() ?? '');
  late final _genres = TextEditingController(text: widget.fields.genres.join('; '));
  bool _toFile = false;

  static List<String> _list(String v) =>
      v.split(';').map((s) => s.trim()).where((s) => s.isNotEmpty).toList();

  static int? _num(String v) => int.tryParse(v.trim());

  void _save() {
    Navigator.of(context).pop((
      TrackFieldsDto(
        title: _title.text.trim().isEmpty ? widget.fields.title : _title.text.trim(),
        artists: _list(_artists.text),
        album: _album.text.trim(),
        albumArtist: _albumArtist.text.trim(),
        year: _num(_year.text),
        trackNo: _num(_trackNo.text),
        discNo: _num(_discNo.text),
        genres: _list(_genres.text),
      ),
      _toFile,
    ));
  }

  Widget _field(TextEditingController c, String label, {bool number = false, String? hint}) => Padding(
    padding: const EdgeInsets.only(bottom: 10),
    child: TextField(
      controller: c,
      keyboardType: number ? TextInputType.number : TextInputType.text,
      decoration: InputDecoration(labelText: label, helperText: hint, border: const OutlineInputBorder(), isDense: true),
    ),
  );

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('Upravit údaje'),
      content: SizedBox(
        width: 460,
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              _field(_title, 'Název'),
              _field(_artists, 'Interpret', hint: 'Více interpretů odděl středníkem'),
              _field(_album, 'Album'),
              _field(_albumArtist, 'Interpret alba'),
              Row(
                children: [
                  Expanded(child: _field(_year, 'Rok', number: true)),
                  const SizedBox(width: 10),
                  Expanded(child: _field(_trackNo, 'Stopa', number: true)),
                  const SizedBox(width: 10),
                  Expanded(child: _field(_discNo, 'Disk', number: true)),
                ],
              ),
              _field(_genres, 'Žánr', hint: 'Více žánrů odděl středníkem'),
              CheckboxListTile(
                contentPadding: EdgeInsets.zero,
                value: _toFile,
                onChanged: (v) => setState(() => _toFile = v ?? false),
                title: const Text('Zapsat i do souboru'),
                subtitle: const Text('Jinak se změny drží jen v knihovně. Opětovné načtení je nepřepíše.'),
              ),
            ],
          ),
        ),
      ),
      actions: [
        TextButton(onPressed: () => Navigator.of(context).pop(), child: const Text('Zrušit')),
        FilledButton(onPressed: _save, child: const Text('Uložit')),
      ],
    );
  }
}
