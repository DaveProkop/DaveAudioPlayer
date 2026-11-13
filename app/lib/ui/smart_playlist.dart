import 'dart:convert';

import 'package:flutter/material.dart';

import '../src/rust/api/playlists.dart';

/// Fields a smart playlist rule can test, with the operators that make sense for them.
const _fields = <String, (String, List<String>)>{
  'artist': ('Interpret', ['contains', 'is', 'is_not']),
  'title': ('Název', ['contains', 'is', 'is_not']),
  'album': ('Album', ['contains', 'is', 'is_not']),
  'genre': ('Žánr', ['is', 'contains', 'is_not']),
  'year': ('Rok', ['at_least', 'at_most', 'is']),
  'rating': ('Hodnocení (hvězdy)', ['at_least', 'at_most', 'is']),
  'play_count': ('Počet přehrání', ['at_least', 'at_most', 'is']),
  'skip_count': ('Počet přeskočení', ['at_most', 'at_least', 'is']),
  'last_played_days': ('Nehráno dní', ['at_least', 'at_most']),
  'added_days': ('Přidáno před dny', ['at_most', 'at_least']),
  'bpm': ('Tempo (BPM)', ['at_least', 'at_most']),
  'key': ('Tónina (Camelot)', ['is', 'is_not']),
};

const _ops = {
  'contains': 'obsahuje',
  'is': 'je',
  'is_not': 'není',
  'at_least': 'aspoň',
  'at_most': 'nejvýš',
};

const _orders = {
  'library': 'Podle knihovny',
  'random': 'Náhodně',
  'most_played': 'Nejhranější',
  'recently_played': 'Naposledy hrané',
  'recently_added': 'Naposledy přidané',
  'rating': 'Nejlépe hodnocené',
};

class _RuleRow {
  _RuleRow(this.field, this.op, String value) : value = TextEditingController(text: value);

  String field;
  String op;
  final TextEditingController value;
}

/// Edits smart playlist rules; returns (name, rules JSON) or null when cancelled.
Future<(String, String)?> showSmartPlaylistEditor(
  BuildContext context, {
  String name = '',
  String? rulesJson,
}) {
  return showDialog<(String, String)>(
    context: context,
    builder: (_) => _SmartEditor(name: name, rulesJson: rulesJson),
  );
}

class _SmartEditor extends StatefulWidget {
  const _SmartEditor({required this.name, this.rulesJson});

  final String name;
  final String? rulesJson;

  @override
  State<_SmartEditor> createState() => _SmartEditorState();
}

class _SmartEditorState extends State<_SmartEditor> {
  late final _name = TextEditingController(text: widget.name);
  final _limit = TextEditingController();
  bool _matchAll = true;
  String _order = 'library';
  final List<_RuleRow> _rules = [];
  int? _count;

  @override
  void initState() {
    super.initState();
    final json = widget.rulesJson;
    if (json != null) {
      final m = jsonDecode(json) as Map<String, dynamic>;
      _matchAll = m['match_all'] as bool? ?? true;
      _order = m['order'] as String? ?? 'library';
      final limit = m['limit'] as int? ?? 0;
      if (limit > 0) _limit.text = '$limit';
      for (final r in (m['rules'] as List? ?? const [])) {
        final rule = r as Map<String, dynamic>;
        _rules.add(_RuleRow(rule['field'] as String, rule['op'] as String, rule['value'] as String));
      }
    }
    if (_rules.isEmpty) _rules.add(_RuleRow('rating', 'at_least', '4'));
    _preview();
  }

  String get _json => jsonEncode({
    'match_all': _matchAll,
    'rules': [
      for (final r in _rules) {'field': r.field, 'op': r.op, 'value': r.value.text},
    ],
    'order': _order,
    'limit': int.tryParse(_limit.text.trim()) ?? 0,
  });

  Future<void> _preview() async {
    try {
      final n = await smartPreviewCount(rulesJson: _json);
      if (mounted) setState(() => _count = n);
    } catch (_) {
      if (mounted) setState(() => _count = null);
    }
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return AlertDialog(
      title: const Text('Chytrý playlist'),
      content: SizedBox(
        width: 560,
        child: SingleChildScrollView(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              TextField(
                controller: _name,
                decoration: const InputDecoration(labelText: 'Název', border: OutlineInputBorder(), isDense: true),
              ),
              const SizedBox(height: 12),
              Row(
                children: [
                  const Text('Skladby, které splňují'),
                  const SizedBox(width: 8),
                  DropdownButton<bool>(
                    value: _matchAll,
                    items: const [
                      DropdownMenuItem(value: true, child: Text('všechna pravidla')),
                      DropdownMenuItem(value: false, child: Text('některé pravidlo')),
                    ],
                    onChanged: (v) {
                      setState(() => _matchAll = v ?? true);
                      _preview();
                    },
                  ),
                ],
              ),
              for (final (i, r) in _rules.indexed)
                Padding(
                  padding: const EdgeInsets.only(top: 6),
                  child: Row(
                    children: [
                      Expanded(
                        flex: 5,
                        child: DropdownButton<String>(
                          isExpanded: true,
                          value: r.field,
                          items: [
                            for (final e in _fields.entries) DropdownMenuItem(value: e.key, child: Text(e.value.$1)),
                          ],
                          onChanged: (v) {
                            if (v == null) return;
                            setState(() {
                              r.field = v;
                              if (!_fields[v]!.$2.contains(r.op)) r.op = _fields[v]!.$2.first;
                            });
                            _preview();
                          },
                        ),
                      ),
                      const SizedBox(width: 8),
                      Expanded(
                        flex: 3,
                        child: DropdownButton<String>(
                          isExpanded: true,
                          value: r.op,
                          items: [
                            for (final op in _fields[r.field]!.$2) DropdownMenuItem(value: op, child: Text(_ops[op]!)),
                          ],
                          onChanged: (v) {
                            if (v == null) return;
                            setState(() => r.op = v);
                            _preview();
                          },
                        ),
                      ),
                      const SizedBox(width: 8),
                      Expanded(
                        flex: 4,
                        child: TextField(
                          controller: r.value,
                          decoration: const InputDecoration(isDense: true),
                          onChanged: (_) => _preview(),
                        ),
                      ),
                      IconButton(
                        tooltip: 'Odebrat pravidlo',
                        onPressed: _rules.length == 1
                            ? null
                            : () {
                                setState(() => _rules.removeAt(i));
                                _preview();
                              },
                        icon: const Icon(Icons.remove_circle_outline),
                      ),
                    ],
                  ),
                ),
              TextButton.icon(
                onPressed: () => setState(() => _rules.add(_RuleRow('artist', 'contains', ''))),
                icon: const Icon(Icons.add),
                label: const Text('Přidat pravidlo'),
              ),
              const SizedBox(height: 8),
              Row(
                children: [
                  const Text('Řadit'),
                  const SizedBox(width: 8),
                  DropdownButton<String>(
                    value: _order,
                    items: [for (final e in _orders.entries) DropdownMenuItem(value: e.key, child: Text(e.value))],
                    onChanged: (v) => setState(() => _order = v ?? 'library'),
                  ),
                  const SizedBox(width: 16),
                  SizedBox(
                    width: 120,
                    child: TextField(
                      controller: _limit,
                      keyboardType: TextInputType.number,
                      decoration: const InputDecoration(labelText: 'Nejvýš skladeb', isDense: true),
                      onChanged: (_) => _preview(),
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 10),
              Text(
                _count == null ? 'Pravidla zatím nedávají smysl' : 'Teď vybírá $_count skladeb',
                style: TextStyle(color: scheme.primary),
              ),
            ],
          ),
        ),
      ),
      actions: [
        TextButton(onPressed: () => Navigator.of(context).pop(), child: const Text('Zrušit')),
        FilledButton(
          onPressed: () {
            final name = _name.text.trim();
            Navigator.of(context).pop((name.isEmpty ? 'Chytrý playlist' : name, _json));
          },
          child: const Text('Uložit'),
        ),
      ],
    );
  }
}
