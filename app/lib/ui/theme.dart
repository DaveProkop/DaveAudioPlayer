import 'dart:io';

import 'package:flutter/material.dart';

import '../player_controller.dart';
import '../src/rust/api/library.dart';
import '../src/rust/api/player.dart' as rust;

/// Accent used when there is no cover (or "colour from cover" is off).
const fallbackAccent = Color(0xFFF0A04B);

/// Colours that cannot come from a [ColorScheme] alone.
@immutable
class AppColors extends ThemeExtension<AppColors> {
  const AppColors({required this.gapless, required this.ok, required this.warn});

  /// Gapless (same album) transition labels.
  final Color gapless;
  final Color ok;
  final Color warn;

  @override
  AppColors copyWith({Color? gapless, Color? ok, Color? warn}) =>
      AppColors(gapless: gapless ?? this.gapless, ok: ok ?? this.ok, warn: warn ?? this.warn);

  @override
  AppColors lerp(AppColors? other, double t) => other == null
      ? this
      : AppColors(
          gapless: Color.lerp(gapless, other.gapless, t)!,
          ok: Color.lerp(ok, other.ok, t)!,
          warn: Color.lerp(warn, other.warn, t)!,
        );
}

extension AppColorsX on BuildContext {
  AppColors get appColors => Theme.of(this).extension<AppColors>()!;
}

ThemeData buildTheme(Color accent, Brightness brightness) {
  final dark = brightness == Brightness.dark;
  final seeded = ColorScheme.fromSeed(seedColor: accent, brightness: brightness);
  final scheme = dark
      ? seeded.copyWith(
          primary: accent,
          onPrimary: _readableOn(accent),
          surface: const Color(0xFF0E1014),
          onSurface: const Color(0xFFE9EBEF),
          onSurfaceVariant: const Color(0xFF9AA1AD),
          surfaceContainerLowest: const Color(0xFF0B0C0F),
          surfaceContainerLow: const Color(0xFF12151A),
          surfaceContainer: const Color(0xFF161920),
          surfaceContainerHigh: const Color(0xFF1D2129),
          surfaceContainerHighest: const Color(0xFF262B35),
          outline: const Color(0xFF6B7280),
          outlineVariant: const Color(0xFF2A2F3A),
        )
      : seeded;
  return ThemeData(
    colorScheme: scheme,
    brightness: brightness,
    useMaterial3: true,
    scaffoldBackgroundColor: scheme.surface,
    visualDensity: VisualDensity.standard,
    dividerTheme: DividerThemeData(color: scheme.outlineVariant, space: 1, thickness: 1),
    sliderTheme: SliderThemeData(
      trackHeight: 4,
      inactiveTrackColor: scheme.surfaceContainerHighest,
      thumbShape: const RoundSliderThumbShape(enabledThumbRadius: 6),
      overlayShape: const RoundSliderOverlayShape(overlayRadius: 14),
      thumbColor: scheme.onSurface,
    ),
    chipTheme: ChipThemeData(
      side: BorderSide.none,
      shape: const StadiumBorder(),
      backgroundColor: scheme.surfaceContainerHigh,
      selectedColor: accent.withValues(alpha: .16),
      labelStyle: TextStyle(color: scheme.onSurfaceVariant, fontSize: 13),
      secondaryLabelStyle: TextStyle(color: accent, fontSize: 13),
      showCheckmark: false,
    ),
    listTileTheme: const ListTileThemeData(contentPadding: EdgeInsets.symmetric(horizontal: 12)),
    extensions: [
      AppColors(
        gapless: dark ? const Color(0xFF6FBF9A) : const Color(0xFF2E8B62),
        ok: dark ? const Color(0xFF6FBF9A) : const Color(0xFF2E8B62),
        warn: dark ? const Color(0xFFE0A95A) : const Color(0xFFB7791F),
      ),
    ],
  );
}

Color _readableOn(Color c) => c.computeLuminance() > .4 ? const Color(0xFF1A1206) : Colors.white;

/// UI preferences stored in the library database (`ui.*` settings).
class UiPrefs extends ChangeNotifier {
  ThemeMode themeMode = ThemeMode.dark;
  bool coverAccent = true;

  Future<void> load() async {
    try {
      themeMode = switch (await uiSetting(key: 'theme')) {
        'light' => ThemeMode.light,
        'system' => ThemeMode.system,
        _ => ThemeMode.dark,
      };
      coverAccent = await uiSetting(key: 'cover_accent') != 'false';
    } catch (_) {
      // Engine not running: defaults.
    }
    notifyListeners();
  }

  void setThemeMode(ThemeMode m) {
    themeMode = m;
    notifyListeners();
    rust.setUiSetting(key: 'theme', value: m.name);
  }

  void setCoverAccent(bool on) {
    coverAccent = on;
    notifyListeners();
    rust.setUiSetting(key: 'cover_accent', value: '$on');
  }
}

/// The accent colour: taken from the current track's cover.
class AccentController extends ValueNotifier<Color> {
  AccentController(this._player, this._prefs) : super(fallbackAccent) {
    _player.addListener(_update);
    _prefs.addListener(_update);
  }

  final PlayerController _player;
  final UiPrefs _prefs;
  String? _cover;
  final Map<String, Color> _cache = {};

  void _update() {
    final TrackInfo? t = _player.currentTrack;
    final cover = _prefs.coverAccent ? t?.coverMedium : null;
    if (cover == _cover) return;
    _cover = cover;
    if (cover == null) {
      value = fallbackAccent;
      return;
    }
    final cached = _cache[cover];
    if (cached != null) {
      value = cached;
      return;
    }
    ColorScheme.fromImageProvider(provider: FileImage(File(cover)), brightness: Brightness.dark).then(
      (scheme) {
        // Keep the accent bright enough to read on the dark background.
        final hsl = HSLColor.fromColor(scheme.primary);
        final c = hsl
            .withLightness(hsl.lightness.clamp(.55, .75))
            .withSaturation(hsl.saturation.clamp(.45, .9))
            .toColor();
        _cache[cover] = c;
        if (_cover == cover) value = c;
      },
      onError: (_) {
        if (_cover == cover) value = fallbackAccent;
      },
    );
  }

  @override
  void dispose() {
    _player.removeListener(_update);
    _prefs.removeListener(_update);
    super.dispose();
  }
}
