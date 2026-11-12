import 'package:flutter/material.dart';

import 'media_session.dart';
import 'platform.dart';
import 'player_controller.dart';
import 'src/rust/frb_generated.dart';
import 'ui/shell.dart';
import 'ui/theme.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();
  final controller = PlayerController();
  final prefs = UiPrefs();
  String? startupError;
  try {
    final dir = await AppPlatform.dataDir();
    await controller.start('$dir/library.db');
    await prefs.load();
    await MediaSession.start(controller);
  } catch (e) {
    startupError = e.toString();
  }
  runApp(DaveAudioPlayerApp(controller: controller, prefs: prefs, startupError: startupError));
}

class DaveAudioPlayerApp extends StatefulWidget {
  const DaveAudioPlayerApp({super.key, required this.controller, required this.prefs, this.startupError});

  final PlayerController controller;
  final UiPrefs prefs;
  final String? startupError;

  @override
  State<DaveAudioPlayerApp> createState() => _DaveAudioPlayerAppState();
}

class _DaveAudioPlayerAppState extends State<DaveAudioPlayerApp> {
  late final AccentController _accent = AccentController(widget.controller, widget.prefs);

  @override
  void dispose() {
    _accent.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: Listenable.merge([_accent, widget.prefs]),
      builder: (context, _) => MaterialApp(
        title: 'DaveAudioPlayer',
        debugShowCheckedModeBanner: false,
        theme: buildTheme(_accent.value, Brightness.light),
        darkTheme: buildTheme(_accent.value, Brightness.dark),
        themeMode: widget.prefs.themeMode,
        // The accent follows the cover; let the change glide.
        themeAnimationDuration: const Duration(milliseconds: 600),
        home: widget.startupError != null
            ? Scaffold(body: Center(child: Text('Nelze spustit přehrávač:\n${widget.startupError}')))
            : AppShell(player: widget.controller, prefs: widget.prefs),
      ),
    );
  }
}
