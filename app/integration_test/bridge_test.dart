// End-to-end check of the Rust core through flutter_rust_bridge on a real device:
// start engine → scan a folder → search → queue → play to the end.
//
//   flutter test integration_test -d windows

import 'dart:io';
import 'dart:math';
import 'dart:typed_data' as td;

import 'package:daveaudioplayer/src/rust/api/library.dart';
import 'package:daveaudioplayer/src/rust/api/player.dart';
import 'package:daveaudioplayer/src/rust/frb_generated.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge.dart' show Int64List;
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

/// One second of a quiet 440 Hz tone, 16-bit mono WAV.
td.Uint8List toneWav() {
  const rate = 44100, frames = 44100;
  final b = td.BytesBuilder();
  void u32(int v) => b.add((td.ByteData(4)..setUint32(0, v, td.Endian.little)).buffer.asUint8List());
  void u16(int v) => b.add((td.ByteData(2)..setUint16(0, v, td.Endian.little)).buffer.asUint8List());
  b.add('RIFF'.codeUnits);
  u32(36 + frames * 2);
  b.add('WAVEfmt '.codeUnits);
  u32(16);
  u16(1);
  u16(1);
  u32(rate);
  u32(rate * 2);
  u16(2);
  u16(16);
  b.add('data'.codeUnits);
  u32(frames * 2);
  final samples = td.ByteData(frames * 2);
  for (var i = 0; i < frames; i++) {
    samples.setInt16(i * 2, (1500 * sin(2 * pi * 440 * i / rate)).round(), td.Endian.little);
  }
  b.add(samples.buffer.asUint8List());
  return b.toBytes();
}

/// A 2x2 24-bit BMP used as the folder cover.
td.Uint8List coverBmp() {
  final b = td.BytesBuilder();
  void u32(int v) => b.add((td.ByteData(4)..setUint32(0, v, td.Endian.little)).buffer.asUint8List());
  void u16(int v) => b.add((td.ByteData(2)..setUint16(0, v, td.Endian.little)).buffer.asUint8List());
  b.add('BM'.codeUnits);
  u32(54 + 16);
  u32(0);
  u32(54);
  u32(40);
  u32(2);
  u32(2);
  u16(1);
  u16(24);
  b.add(List.filled(24, 0));
  b.add([0, 0, 255, 0, 255, 0, 0, 0, 255, 0, 0, 255, 255, 255, 0, 0]);
  return b.toBytes();
}

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('Rust core works through the bridge', (tester) async {
    await tester.runAsync(() async {
      // ignore: avoid_print
      void step(String s) => print('[bridge] $s');
      step('init');
      await RustLib.init();
      final dir = await Directory.systemTemp.createTemp('dap_bridge_');
      final music = await Directory('${dir.path}/music').create();
      await File('${music.path}/tone.wav').writeAsBytes(toneWav());
      await File('${music.path}/cover.bmp').writeAsBytes(coverBmp());

      final events = <AppEvent>[];
      step('start engine');
      await startEngine(dbPath: '${dir.path}/library.db');
      final sub = appEvents().listen(events.add);

      Future<AppEvent> waitFor(bool Function(AppEvent) match, {int from = 0}) async {
        final deadline = DateTime.now().add(const Duration(seconds: 20));
        while (DateTime.now().isBefore(deadline)) {
          for (final e in events.skip(from)) {
            if (match(e)) return e;
          }
          await Future<void>.delayed(const Duration(milliseconds: 50));
        }
        fail('timed out; got ${events.map((e) => '${e.kind} ${e.state?.status} ${e.message ?? ''}').toList()}');
      }

      step('settings');
      final defaults = await waitFor((e) => e.kind == AppEventKind.settings);
      expect(defaults.settings!.crossfadeMs, 6000);
      expect(defaults.settings!.normalize, isTrue);
      final settingsMark = events.length;
      await setCrossfade(ms: 3000);
      final changed = await waitFor((e) => e.kind == AppEventKind.settings, from: settingsMark);
      expect(changed.settings!.crossfadeMs, 3000);

      step('scan');
      await addFolder(path: music.path);
      final finished = await waitFor((e) => e.kind == AppEventKind.scanFinished);
      expect(finished.scanSummary!.added, 1);
      expect(await trackCount(), 1);

      step('search');
      final hits = await search(query: 'ton', limit: 10);
      expect(hits.map((t) => t.title), ['tone']);
      expect(hits.single.durationMs, inInclusiveRange(990, 1010));
      final t = hits.single;
      for (final cover in [t.coverSmall, t.coverMedium, t.coverLarge]) {
        expect(cover, isNotNull);
        expect(File(cover!).existsSync(), isTrue, reason: cover);
      }

      step('queue and play');
      final mark = events.length;
      await enqueue(trackIds: Int64List.fromList([hits.single.id]));
      final queued = await waitFor((e) => e.kind == AppEventKind.queue, from: mark);
      expect(queued.queue!.single.trackId, hits.single.id);

      await playIndex(index: 0);
      final playing = await waitFor(
          (e) => e.kind == AppEventKind.state && e.state!.status == PlayStatus.playing,
          from: mark);
      expect(playing.state!.trackId, hits.single.id);
      expect(playing.state!.rate, 1.0);
      step('playing');
      final played = events.length;
      await waitFor(
          (e) => e.kind == AppEventKind.state && e.state!.status == PlayStatus.stopped,
          from: played);

      step('stop');
      // Stopping the engine ends the event stream from the Rust side; cancelling a live
      // bridge stream first would wait for an event that never comes.
      await stopEngine();
      step('engine stopped');
      await sub.cancel();
      step('done');
      await dir.delete(recursive: true);
    });
  }, timeout: const Timeout(Duration(minutes: 2)));
}
