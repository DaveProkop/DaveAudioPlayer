// Drives the real app (Rust engine included) through its main screens on a generated
// library and saves a PNG of each, for checking the UI by eye:
//
//   flutter test integration_test/ui_screens_test.dart -d windows
//
// Screenshots go to the folder in DAP_SHOTS (default: <temp>/dap_shots).

import 'dart:convert';
import 'dart:io';
import 'dart:math';
import 'dart:typed_data' as td;
import 'dart:ui' as ui;

import 'package:daveaudioplayer/main.dart';
import 'package:daveaudioplayer/player_controller.dart';
import 'package:daveaudioplayer/src/rust/api/library.dart';
import 'package:daveaudioplayer/src/rust/api/player.dart';
import 'package:daveaudioplayer/src/rust/api/playlists.dart';
import 'package:daveaudioplayer/src/rust/frb_generated.dart';
import 'package:daveaudioplayer/ui/player.dart' show MiniPlayer;
import 'package:daveaudioplayer/ui/theme.dart';
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge.dart' show Int64List;
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

td.Uint8List _u32(int v) => (td.ByteData(4)..setUint32(0, v, td.Endian.little)).buffer.asUint8List();
td.Uint8List _u16(int v) => (td.ByteData(2)..setUint16(0, v, td.Endian.little)).buffer.asUint8List();

List<int> _syncsafe(int v) => [(v >> 21) & 0x7f, (v >> 14) & 0x7f, (v >> 7) & 0x7f, v & 0x7f];

/// A minimal ID3v2.4 tag with UTF-8 text frames.
List<int> _id3(Map<String, String> frames) {
  final body = <int>[];
  frames.forEach((id, text) {
    final data = [3, ...utf8.encode(text)];
    body
      ..addAll(ascii.encode(id))
      ..addAll(_syncsafe(data.length))
      ..addAll([0, 0])
      ..addAll(data);
  });
  return [...ascii.encode('ID3'), 4, 0, 0, ..._syncsafe(body.length), ...body];
}

/// A short stereo WAV tone with an ID3 chunk (lofty reads tags from it).
td.Uint8List _wav(double seconds, double freq, Map<String, String> tags) {
  const rate = 44100;
  final frames = (rate * seconds).round();
  final pcm = td.ByteData(frames * 4);
  for (var i = 0; i < frames; i++) {
    final s = (2500 * sin(2 * pi * freq * i / rate)).round();
    pcm.setInt16(i * 4, s, td.Endian.little);
    pcm.setInt16(i * 4 + 2, s, td.Endian.little);
  }
  final tag = _id3(tags);
  final tagPad = tag.length.isOdd ? 1 : 0;
  final b = td.BytesBuilder()
    ..add(ascii.encode('RIFF'))
    ..add(_u32(4 + 24 + 8 + frames * 4 + 8 + tag.length + tagPad))
    ..add(ascii.encode('WAVEfmt '))
    ..add(_u32(16))
    ..add(_u16(1))
    ..add(_u16(2))
    ..add(_u32(rate))
    ..add(_u32(rate * 4))
    ..add(_u16(4))
    ..add(_u16(16))
    ..add(ascii.encode('data'))
    ..add(_u32(frames * 4))
    ..add(pcm.buffer.asUint8List())
    ..add(ascii.encode('id3 '))
    ..add(_u32(tag.length))
    ..add(tag);
  if (tagPad == 1) b.add([0]);
  return b.toBytes();
}

/// A 96x96 24-bit BMP with a diagonal gradient between two colours.
td.Uint8List _coverBmp(Color a, Color c) {
  const n = 96;
  final row = n * 3; // multiple of 4
  final b = td.BytesBuilder()
    ..add(ascii.encode('BM'))
    ..add(_u32(54 + row * n))
    ..add(_u32(0))
    ..add(_u32(54))
    ..add(_u32(40))
    ..add(_u32(n))
    ..add(_u32(n))
    ..add(_u16(1))
    ..add(_u16(24))
    ..add(List.filled(24, 0));
  for (var y = 0; y < n; y++) {
    for (var x = 0; x < n; x++) {
      final t = (x + (n - y)) / (2 * n);
      final col = Color.lerp(a, c, t)!;
      b.add([(col.b * 255).round(), (col.g * 255).round(), (col.r * 255).round()]);
    }
  }
  return b.toBytes();
}

const _library = [
  ('Polární záře', 'Letní noc', 2019, [Color(0xFFF7B267), Color(0xFF5B2A86)],
      ['Svítání nad městem', 'Ostrov', 'Odraz', 'Vlny']),
  ('Anna Vlčková', 'Proudy', 2021, [Color(0xFF1B998B), Color(0xFF2D3047)], ['Druhý břeh', 'Ticho po bouři', 'Mělčina']),
  ('Kvadrant', 'Signály', 2023, [Color(0xFF3A86FF), Color(0xFF0B132B)], ['Neon', 'Pulse 128', 'Hvězdný prach']),
  ('Rezavý Kůň', 'Silnice 9', 2016, [Color(0xFFFB8B24), Color(0xFF5F0F40)], ['Prach', 'Ocelové nebe']),
  ('Mira Sol', 'Pobřeží', 2020, [Color(0xFFE9C46A), Color(0xFF2A9D8F)], ['Teplé světlo', 'Noční vlak']),
];

Future<void> _writeLibrary(Directory music) async {
  var f = 220.0;
  const keys = ['8A', '9A', '8B', '3A', '10B', '7A', '9B', '2A'];
  var k = 0;
  for (final (artist, album, year, colors, titles) in _library) {
    final dir = await Directory('${music.path}/$artist/$album').create(recursive: true);
    await File('${dir.path}/cover.bmp').writeAsBytes(_coverBmp(colors[0], colors[1]));
    for (final (i, title) in titles.indexed) {
      f *= 1.06;
      await File('${dir.path}/${i + 1} $title.wav').writeAsBytes(_wav(4 + i.toDouble(), f, {
        'TIT2': title,
        'TPE1': artist,
        'TPE2': artist,
        'TALB': album,
        'TRCK': '${i + 1}/${titles.length}',
        'TDRC': '$year',
        'TKEY': keys[k % keys.length],
        'TBPM': '${[124, 128, 126, 174, 122, 140, 125, 172][k++ % 8]}',
      }));
      // Synced lyrics next to the first album's tracks.
      if (album == 'Letní noc') {
        await File('${dir.path}/${i + 1} $title.lrc').writeAsString(
          '[00:00.00]$title\n[00:01.00]Světla města zhasínají\n[00:02.00]a noc je ještě mladá\n'
          '[00:03.00]vlny šumí pod mostem\n[00:04.00]a ráno nikdo nehledá\n',
        );
      }
    }
  }
}

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('main screens render with a real library', (tester) async {
    final out = Directory(Platform.environment['DAP_SHOTS'] ?? '${Directory.systemTemp.path}/dap_shots');
    late Directory dir;
    final player = PlayerController();
    final prefs = UiPrefs();

    await tester.runAsync(() async {
      await RustLib.init();
      out.createSync(recursive: true);
      dir = await Directory.systemTemp.createTemp('dap_ui_');
      final music = await Directory('${dir.path}/music').create();
      await _writeLibrary(music);
      await player.start('${dir.path}/library.db');
      await prefs.load();
      await addFolder(path: music.path);
      final deadline = DateTime.now().add(const Duration(seconds: 30));
      while (player.libraryVersion == 0 && DateTime.now().isBefore(deadline)) {
        await Future<void>.delayed(const Duration(milliseconds: 100));
      }
    });
    expect(player.libraryVersion, greaterThan(0), reason: 'scan finished');

    Future<void> settle() async {
      for (var i = 0; i < 12; i++) {
        await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 60)));
        await tester.pump(const Duration(milliseconds: 60));
      }
    }

    Future<void> shot(String name) async {
      await settle();
      await tester.runAsync(() async {
        final view = binding.renderViews.first;
        final layer = view.debugLayer! as OffsetLayer;
        final image = await layer.toImage(Offset.zero & view.size);
        final png = await image.toByteData(format: ui.ImageByteFormat.png);
        await File('${out.path}/$name.png').writeAsBytes(png!.buffer.asUint8List());
      });
    }

    Future<void> tapText(String text) async {
      await tester.tap(find.text(text).first);
      await settle();
    }

    // No online lookups from tests.
    await tester.runAsync(() async {
      await setUiSetting(key: 'lyrics_online', value: 'false');
      await Future<void>.delayed(const Duration(milliseconds: 200));
    });

    // ----- desktop -----
    tester.view.devicePixelRatio = 1;
    tester.view.physicalSize = const Size(1400, 860);
    await tester.pumpWidget(DaveAudioPlayerApp(controller: player, prefs: prefs));
    await settle();
    await shot('desktop_tracks');

    // Play an album from its page so the queue, player bar and accent fill in.
    await tapText('Alba');
    await shot('desktop_albums');
    await tapText('Letní noc');
    await tester.runAsync(() async {
      await player.playAll(await _albumTracks(player, 'Letní noc'));
      await Future<void>.delayed(const Duration(milliseconds: 800));
    });
    await shot('desktop_album');
    await tapText('Právě hraje');
    await shot('desktop_now_playing');
    await tapText('Ekvalizér');
    await shot('desktop_eq');
    await tapText('Nastavení');
    await shot('desktop_settings');
    await tapText('Interpreti');
    await shot('desktop_artists');

    // Playlists: one made from an album.
    await tester.runAsync(() async {
      final tracks = await _albumTracks(player, 'Letní noc');
      await createPlaylist(name: 'Na večer', trackIds: Int64List.fromList([for (final t in tracks) t.id]));
    });
    await tapText('Playlisty');
    await shot('desktop_playlists');
    await tapText('Na večer');
    await shot('desktop_playlist');
    await tester.runAsync(() async {
      await createSmartPlaylist(
        name: 'Rychlé',
        rulesJson: '{"match_all": true, "rules": [{"field": "bpm", "op": "at_least", "value": "140"}], "order": "library", "limit": 0}',
      );
    });
    player.libraryEdited();
    await tapText('Playlisty');
    await tapText('Rychlé');
    await shot('desktop_smart_playlist');
    await tapText('Statistiky');
    await shot('desktop_stats');
    await tapText('Složky');
    await tester.tap(find.byIcon(Icons.folder_outlined).last);
    await settle();
    await tester.tap(find.widgetWithText(ListTile, 'Polární záře'));
    await settle();
    await shot('desktop_folders');

    // Smart Mix: the whole library in the queue, then the proposed order.
    await tester.runAsync(() async {
      final all = await libraryPage(offset: 0, limit: 100);
      await player.playAll(all);
      await Future<void>.delayed(const Duration(milliseconds: 800));
    });
    await settle();
    await tester.tap(find.byIcon(Icons.auto_awesome).first);
    await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 500)));
    await shot('desktop_mix');
    await tapText('Použít pořadí');
    await shot('desktop_mix_applied');

    // ----- mobile -----
    tester.view.physicalSize = const Size(412, 900);
    await settle();
    await shot('mobile_library');
    await tapText('Fronta');
    await shot('mobile_queue');
    await tester.tap(find.byType(MiniPlayer));
    await settle();
    await shot('mobile_now_playing');

    await tester.pumpWidget(const SizedBox());
    tester.view.reset();
    await tester.runAsync(() async {
      await stopEngine();
      await Future<void>.delayed(const Duration(milliseconds: 200));
      try {
        await dir.delete(recursive: true);
      } catch (_) {}
    });
    // ignore: avoid_print
    print('[ui] screenshots in ${out.path}');
  }, timeout: const Timeout(Duration(minutes: 3)));
}

Future<List<TrackInfo>> _albumTracks(PlayerController player, String title) async {
  final album = (await albums()).firstWhere((a) => a.title == title);
  return albumTracks(albumId: album.id);
}
