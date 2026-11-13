import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../player_controller.dart';
import '../src/rust/api/stats.dart';
import 'widgets.dart';

/// Listening statistics: totals, a plays-per-day chart, top tracks, artists and genres.
class StatsPage extends StatefulWidget {
  const StatsPage({super.key, required this.player});

  final PlayerController player;

  @override
  State<StatsPage> createState() => _StatsPageState();
}

class _StatsPageState extends State<StatsPage> {
  static const _periods = [(7, '7 dní'), (30, '30 dní'), (365, 'Rok'), (0, 'Celkem')];
  int _days = 30;
  late Future<ListenStats> _stats = listenStats(days: _days);

  void _period(int days) => setState(() {
    _days = days;
    _stats = listenStats(days: days);
  });

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return FutureBuilder<ListenStats>(
      future: _stats,
      builder: (context, snap) {
        final s = snap.data;
        return ListView(
          padding: const EdgeInsets.fromLTRB(16, 12, 16, 24),
          children: [
            if (Navigator.of(context).canPop())
              Align(
                alignment: Alignment.centerLeft,
                child: TextButton.icon(
                  onPressed: () => Navigator.of(context).pop(),
                  icon: const Icon(Icons.arrow_back),
                  label: const Text('Zpět'),
                ),
              ),
            PageHeader(
              title: 'Statistiky',
              subtitle: 'Co posloucháš nejvíc',
              trailing: SegmentedButton<int>(
                segments: [for (final (d, label) in _periods) ButtonSegment(value: d, label: Text(label))],
                selected: {_days},
                showSelectedIcon: false,
                onSelectionChanged: (v) => _period(v.first),
              ),
            ),
            if (s == null)
              const Padding(padding: EdgeInsets.all(40), child: Center(child: CircularProgressIndicator()))
            else if (s.plays == 0 && s.skips == 0)
              const EmptyState(icon: Icons.insights, text: 'Za toto období zatím nic.\nPusť si něco a vrať se.')
            else ...[
              Wrap(
                spacing: 10,
                runSpacing: 10,
                children: [
                  _Tile(value: '${s.plays}', label: 'přehrání'),
                  _Tile(value: formatLong(s.listenedMs.toInt()), label: 'poslechu'),
                  _Tile(value: '${s.distinctTracks}', label: 'různých skladeb'),
                  _Tile(value: '${s.skips}', label: 'přeskočeno'),
                ],
              ),
              const SizedBox(height: 16),
              _Card(
                title: 'Přehrání po dnech',
                child: SizedBox(
                  height: 120,
                  child: CustomPaint(
                    size: Size.infinite,
                    painter: _BarsPainter([for (final n in s.perDay) n], scheme.primary, scheme.outlineVariant),
                  ),
                ),
              ),
              LayoutBuilder(
                builder: (context, c) {
                  final cols = c.maxWidth >= 900 ? 3 : (c.maxWidth >= 600 ? 2 : 1);
                  final cards = [
                    _Card(
                      title: 'Nejhranější skladby',
                      child: _TrackList(items: [for (final t in s.topTracks) (t.track, '${t.count}×')], player: widget.player),
                    ),
                    _Card(title: 'Interpreti', child: _Bars(items: [for (final a in s.topArtists) (a.name, a.count)])),
                    _Card(title: 'Žánry', child: _Bars(items: [for (final g in s.topGenres) (g.name, g.count)])),
                    if (s.mostSkipped.isNotEmpty)
                      _Card(
                        title: 'Nejčastěji přeskakované',
                        child: _TrackList(
                          items: [for (final t in s.mostSkipped) (t.track, '${t.count}×')],
                          player: widget.player,
                        ),
                      ),
                    _Card(
                      title: 'Naposledy',
                      child: _TrackList(
                        items: [
                          for (final r in s.recent)
                            (r.track, '${_ago(r.atMs.toInt())}${r.skipped ? ' · přeskočeno' : ''}'),
                        ],
                        player: widget.player,
                      ),
                    ),
                  ];
                  final width = (c.maxWidth - (cols - 1) * 12) / cols;
                  return Wrap(
                    spacing: 12,
                    children: [for (final card in cards) SizedBox(width: width, child: card)],
                  );
                },
              ),
            ],
          ],
        );
      },
    );
  }

  static String _ago(int atMs) {
    final d = DateTime.now().difference(DateTime.fromMillisecondsSinceEpoch(atMs));
    if (d.inMinutes < 1) return 'teď';
    if (d.inHours < 1) return 'před ${d.inMinutes} min';
    if (d.inDays < 1) return 'před ${d.inHours} h';
    return 'před ${plural(d.inDays, 'dnem', 'dny', 'dny')}';
  }
}

class _Tile extends StatelessWidget {
  const _Tile({required this.value, required this.label});

  final String value;
  final String label;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      width: 170,
      padding: const EdgeInsets.all(14),
      decoration: BoxDecoration(color: scheme.surfaceContainer, borderRadius: BorderRadius.circular(12)),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(value, style: const TextStyle(fontSize: 24, fontWeight: FontWeight.w700)),
          Text(label, style: TextStyle(color: scheme.onSurfaceVariant)),
        ],
      ),
    );
  }
}

class _Card extends StatelessWidget {
  const _Card({required this.title, required this.child});

  final String title;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      margin: const EdgeInsets.only(bottom: 12),
      padding: const EdgeInsets.all(14),
      decoration: BoxDecoration(color: scheme.surfaceContainer, borderRadius: BorderRadius.circular(12)),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(title.toUpperCase(), style: TextStyle(fontSize: 11, letterSpacing: .8, color: scheme.outline)),
          const SizedBox(height: 10),
          child,
        ],
      ),
    );
  }
}

class _TrackList extends StatelessWidget {
  const _TrackList({required this.items, required this.player});

  final List<(dynamic, String)> items;
  final PlayerController player;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    if (items.isEmpty) return Text('—', style: TextStyle(color: scheme.outline));
    return Column(
      children: [
        for (final (t, note) in items)
          InkWell(
            borderRadius: BorderRadius.circular(6),
            onTap: () => player.playNow(t),
            child: Padding(
              padding: const EdgeInsets.symmetric(vertical: 4),
              child: Row(
                children: [
                  CoverImage(path: t.coverSmall, size: 36),
                  const SizedBox(width: 10),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(t.title, maxLines: 1, overflow: TextOverflow.ellipsis),
                        Text(
                          t.artist,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: TextStyle(fontSize: 12, color: scheme.onSurfaceVariant),
                        ),
                      ],
                    ),
                  ),
                  Text(note, style: TextStyle(fontSize: 12, color: scheme.outline)),
                ],
              ),
            ),
          ),
      ],
    );
  }
}

/// Names with horizontal bars proportional to their counts.
class _Bars extends StatelessWidget {
  const _Bars({required this.items});

  final List<(String, int)> items;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    if (items.isEmpty) return Text('—', style: TextStyle(color: scheme.outline));
    final top = items.map((i) => i.$2).fold(1, math.max);
    return Column(
      children: [
        for (final (name, n) in items)
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 4),
            child: Row(
              children: [
                SizedBox(width: 130, child: Text(name, maxLines: 1, overflow: TextOverflow.ellipsis)),
                Expanded(
                  child: ClipRRect(
                    borderRadius: BorderRadius.circular(3),
                    child: LinearProgressIndicator(value: n / top, minHeight: 8, backgroundColor: scheme.surfaceContainerHighest),
                  ),
                ),
                SizedBox(width: 44, child: Text('$n', textAlign: TextAlign.right)),
              ],
            ),
          ),
      ],
    );
  }
}

class _BarsPainter extends CustomPainter {
  _BarsPainter(this.values, this.color, this.base);

  final List<int> values;
  final Color color;
  final Color base;

  @override
  void paint(Canvas canvas, Size size) {
    if (values.isEmpty) return;
    final top = values.fold(1, math.max);
    final w = size.width / values.length;
    final fill = Paint()..color = color;
    canvas.drawLine(Offset(0, size.height), Offset(size.width, size.height), Paint()..color = base);
    for (final (i, v) in values.indexed) {
      if (v == 0) continue;
      final h = (size.height - 4) * v / top;
      canvas.drawRRect(
        RRect.fromRectAndRadius(Rect.fromLTWH(i * w + w * .15, size.height - h, w * .7, h), const Radius.circular(2)),
        fill,
      );
    }
  }

  @override
  bool shouldRepaint(_BarsPainter old) => old.values != values || old.color != color;
}
