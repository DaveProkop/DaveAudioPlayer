import 'dart:convert';
import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../player_controller.dart';
import '../src/rust/api/library.dart' show uiSetting;
import '../src/rust/api/player.dart' as rust;
import 'playlists.dart' show promptPlaylistName;
import 'widgets.dart';

const _bands = ['31', '62', '125', '250', '500', '1k', '2k', '4k', '8k', '16k'];

/// Built-in presets: ten band gains in dB.
const eqPresets = <String, List<double>>{
  'Vypnuto': [0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
  'Rock': [4.5, 3, 1, -1.5, -2.5, -1, 1.5, 3, 4, 3.5],
  'Pop': [-1, 1, 3, 4, 3, 1, -0.5, -1, -0.5, 0],
  'Elektronika': [5, 4, 1.5, 0, -1.5, 0, 1, 2, 3.5, 4],
  'Akustika': [3, 2.5, 1.5, 0.5, 1, 1.5, 2.5, 3, 2.5, 1.5],
  'Basy +': [6, 5, 3.5, 1.5, 0, 0, 0, 0, 0, 0],
  'Výšky +': [0, 0, 0, 0, 0, 0.5, 2, 4, 5.5, 6],
  'Hlas': [-2, -1.5, 0, 2, 3.5, 3.5, 2.5, 1, 0, -1],
};

/// The ten-band equalizer. Changes go to the engine as you drag (it ramps them, no clicks)
/// and are stored there.
class EqPage extends StatefulWidget {
  const EqPage({super.key, required this.player, this.showBack = false});

  final PlayerController player;
  final bool showBack;

  @override
  State<EqPage> createState() => _EqPageState();
}

/// Where the user's own presets are stored: JSON `{name: {"gains": [...], "preamp": x}}`.
const _userPresetsKey = 'eq_user_presets';

class _EqPageState extends State<EqPage> {
  List<double> _gains = List.filled(10, 0);
  double _preamp = 0;
  bool _enabled = false;
  bool _editing = false;
  Map<String, (List<double>, double)> _user = {};

  @override
  void initState() {
    super.initState();
    widget.player.addListener(_sync);
    _sync();
    _loadUserPresets();
  }

  Future<void> _loadUserPresets() async {
    final raw = await uiSetting(key: _userPresetsKey);
    if (raw == null || !mounted) return;
    try {
      final map = (jsonDecode(raw) as Map<String, dynamic>).map((name, v) {
        final m = v as Map<String, dynamic>;
        return MapEntry(name, (
          [for (final g in m['gains'] as List) (g as num).toDouble()],
          (m['preamp'] as num?)?.toDouble() ?? 0.0,
        ));
      });
      setState(() => _user = map);
    } on Object {
      // A damaged value only loses the user presets, not the page.
    }
  }

  void _storeUserPresets() {
    final json = jsonEncode({
      for (final e in _user.entries) e.key: {'gains': e.value.$1, 'preamp': e.value.$2},
    });
    rust.setUiSetting(key: _userPresetsKey, value: json);
  }

  Future<void> _saveUserPreset() async {
    final name = await promptPlaylistName(context, title: 'Uložit preset', initial: _preset ?? '');
    if (name == null || eqPresets.containsKey(name)) return;
    setState(() => _user = {..._user, name: ([..._gains], _preamp)});
    _storeUserPresets();
  }

  void _deleteUserPreset(String name) {
    setState(() => _user = {..._user}..remove(name));
    _storeUserPresets();
  }

  @override
  void dispose() {
    widget.player.removeListener(_sync);
    super.dispose();
  }

  /// Takes the engine's values, except while the user is dragging.
  void _sync() {
    final s = widget.player.settings;
    if (s == null || _editing) return;
    setState(() {
      _gains = [for (final g in s.eqGainsDb) g];
      _preamp = s.eqPreampDb;
      _enabled = s.eqEnabled;
    });
  }

  void _send() => rust.setEq(gainsDb: _gains, preampDb: _preamp);

  void _apply(List<double> gains, {double? preamp}) {
    setState(() {
      _gains = [...gains];
      if (preamp != null) _preamp = preamp;
    });
    _send();
    if (!_enabled) _setEnabled(true);
  }

  void _setEnabled(bool on) {
    setState(() => _enabled = on);
    rust.setEqEnabled(enabled: on);
  }

  bool _same(List<double> gains) => gains.indexed.every((p) => (p.$2 - _gains[p.$1]).abs() < .05);

  String? get _preset {
    for (final e in _user.entries) {
      if (_same(e.value.$1) && (e.value.$2 - _preamp).abs() < .05) return e.key;
    }
    for (final e in eqPresets.entries) {
      if (_same(e.value)) return e.key;
    }
    return null;
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final preset = _preset;
    return ListView(
      padding: const EdgeInsets.fromLTRB(16, 12, 16, 24),
      children: [
        if (widget.showBack)
          Align(
            alignment: Alignment.centerLeft,
            child: TextButton.icon(
              onPressed: () => Navigator.of(context).pop(),
              icon: const Icon(Icons.arrow_back),
              label: const Text('Zpět'),
            ),
          ),
        const PageHeader(
          title: 'Ekvalizér',
          subtitle: 'Platí pro všechno, co hraje. Změny se projeví hned, plynule a bez lupnutí.',
        ),
        Container(
          padding: const EdgeInsets.fromLTRB(16, 14, 16, 16),
          decoration: BoxDecoration(color: scheme.surfaceContainer, borderRadius: BorderRadius.circular(14)),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Switch(value: _enabled, onChanged: _setEnabled),
                  const SizedBox(width: 8),
                  Text(_enabled ? 'Zapnuto' : 'Vypnuto', style: const TextStyle(fontWeight: FontWeight.w600)),
                  const Spacer(),
                  TextButton(onPressed: _saveUserPreset, child: const Text('Uložit…')),
                  TextButton(onPressed: () => _apply(List.filled(10, 0), preamp: 0), child: const Text('Vynulovat')),
                ],
              ),
              const SizedBox(height: 6),
              Wrap(
                spacing: 6,
                runSpacing: 6,
                children: [
                  for (final name in eqPresets.keys)
                    ChoiceChip(
                      label: Text(name),
                      selected: preset == name,
                      onSelected: (_) => _apply(eqPresets[name]!),
                    ),
                  for (final e in _user.entries)
                    InputChip(
                      label: Text(e.key),
                      selected: preset == e.key,
                      onSelected: (_) => _apply(e.value.$1, preamp: e.value.$2),
                      onDeleted: () => _deleteUserPreset(e.key),
                      deleteButtonTooltipMessage: 'Smazat preset',
                    ),
                ],
              ),
              const SizedBox(height: 10),
              SizedBox(
                height: 110,
                child: CustomPaint(
                  size: Size.infinite,
                  painter: _CurvePainter(
                    gains: _gains,
                    preamp: _preamp,
                    color: _enabled ? scheme.primary : scheme.outline,
                    grid: scheme.outlineVariant,
                  ),
                ),
              ),
              const SizedBox(height: 8),
              LayoutBuilder(
                builder: (context, c) {
                  final compact = c.maxWidth < 520;
                  return Row(
                    crossAxisAlignment: CrossAxisAlignment.end,
                    children: [
                      if (!compact) ...[
                        _BandSlider(
                          label: 'Preamp',
                          value: _preamp,
                          color: scheme.outline,
                          compact: compact,
                          onStart: () => _editing = true,
                          onChanged: (v) {
                            setState(() => _preamp = v);
                            _send();
                          },
                          onEnd: () => _editing = false,
                        ),
                        const SizedBox(width: 8),
                      ],
                      for (var i = 0; i < 10; i++)
                        Expanded(
                          child: _BandSlider(
                            label: _bands[i],
                            value: _gains[i],
                            color: _enabled ? scheme.primary : scheme.outline,
                            compact: compact,
                            onStart: () => _editing = true,
                            onChanged: (v) {
                              setState(() => _gains = [..._gains]..[i] = v);
                              _send();
                            },
                            onEnd: () => _editing = false,
                          ),
                        ),
                    ],
                  );
                },
              ),
              LayoutBuilder(
                builder: (context, c) {
                  if (c.maxWidth >= 520) return const SizedBox.shrink();
                  return Padding(
                    padding: const EdgeInsets.only(top: 12),
                    child: Row(
                      children: [
                        const Text('Preamp'),
                        Expanded(
                          child: Slider(
                            value: _preamp,
                            min: -12,
                            max: 12,
                            divisions: 48,
                            label: _db(_preamp),
                            onChanged: (v) {
                              setState(() => _preamp = v);
                              _send();
                            },
                          ),
                        ),
                        SizedBox(width: 52, child: Text(_db(_preamp), textAlign: TextAlign.right)),
                      ],
                    ),
                  );
                },
              ),
            ],
          ),
        ),
      ],
    );
  }
}

String _db(double v) => '${v > 0 ? '+' : ''}${v.toStringAsFixed(1).replaceAll('.', ',')}';

class _BandSlider extends StatelessWidget {
  const _BandSlider({
    required this.label,
    required this.value,
    required this.color,
    required this.compact,
    required this.onStart,
    required this.onChanged,
    required this.onEnd,
  });

  final String label;
  final double value;
  final Color color;
  final bool compact;
  final VoidCallback onStart;
  final ValueChanged<double> onChanged;
  final VoidCallback onEnd;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final small = TextStyle(fontSize: compact ? 10 : 11.5, color: scheme.onSurfaceVariant);
    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(
          _db(value),
          style: small.copyWith(color: scheme.onSurface, fontFeatures: const [FontFeature.tabularFigures()]),
        ),
        SizedBox(
          height: compact ? 160 : 190,
          width: compact ? 30 : 48,
          child: RotatedBox(
            quarterTurns: 3,
            child: SliderTheme(
              data: SliderTheme.of(context).copyWith(
                // Gains go both ways from 0 dB; a fill from −12 dB would mislead. The curve
                // above shows the shape, the thumb carries the colour.
                activeTrackColor: scheme.surfaceContainerHighest,
                inactiveTrackColor: scheme.surfaceContainerHighest,
                thumbColor: color,
                trackHeight: compact ? 4 : 5,
                thumbShape: RoundSliderThumbShape(enabledThumbRadius: compact ? 7 : 9),
              ),
              child: Slider(
                value: value.clamp(-12, 12),
                min: -12,
                max: 12,
                divisions: 48,
                onChangeStart: (_) => onStart(),
                onChanged: onChanged,
                onChangeEnd: (_) => onEnd(),
              ),
            ),
          ),
        ),
        Text(label, style: small),
      ],
    );
  }
}

/// The combined response, computed by the engine's own filters.
class _CurvePainter extends CustomPainter {
  _CurvePainter({required this.gains, required this.preamp, required this.color, required this.grid});

  final List<double> gains;
  final double preamp;
  final Color color;
  final Color grid;

  static final _freqs = [for (var i = 0; i < 120; i++) 20 * math.pow(1000, i / 119).toDouble()];

  @override
  void paint(Canvas canvas, Size size) {
    final db = rust.eqResponseDb(gainsDb: gains, preampDb: preamp, freqsHz: _freqs);
    double y(double v) => size.height / 2 - v.clamp(-18, 18) / 18 * (size.height / 2 - 4);
    canvas.drawLine(Offset(0, size.height / 2), Offset(size.width, size.height / 2), Paint()..color = grid);
    final path = Path();
    for (var i = 0; i < db.length; i++) {
      final p = Offset(i / (db.length - 1) * size.width, y(db[i]));
      i == 0 ? path.moveTo(p.dx, p.dy) : path.lineTo(p.dx, p.dy);
    }
    final fill = Path.from(path)
      ..lineTo(size.width, size.height / 2)
      ..lineTo(0, size.height / 2)
      ..close();
    canvas.drawPath(fill, Paint()..color = color.withValues(alpha: .12));
    canvas.drawPath(
      path,
      Paint()
        ..color = color
        ..style = PaintingStyle.stroke
        ..strokeWidth = 3,
    );
  }

  @override
  bool shouldRepaint(_CurvePainter old) => old.preamp != preamp || old.color != color || !_same(old.gains, gains);

  static bool _same(List<double> a, List<double> b) {
    for (var i = 0; i < a.length; i++) {
      if (a[i] != b[i]) return false;
    }
    return a.length == b.length;
  }
}
