# DaveAudioPlayer

Hudební přehrávač pro **Windows (PC)** a **Android**. Přehrává hudbu z lokálních složek,
které vybere uživatel. Rozpracované MVP: Rust jádro (`rust/`) a ověřovací Flutter UI (`app/`).

## Komunikace
- S uživatelem mluv česky, stručně a věcně.
- Identifikátory v kódu, názvy crate a commit zprávy piš anglicky. Komentáře v kódu anglicky.
- Před většími rozhodnutími (nová závislost, změna architektury, změna schématu DB) se zeptej.

## Dokumentace (přečti před prací)
- `docs/01-koncept.md` – funkce, rozsah, Smart Mix (chytré přechody)
- `docs/02-architektura.md` – vlákna, crate, API Rust ↔ Flutter, platformní specifika
- `docs/03-databaze.md` – zdůvodnění návrhu databáze
- `db/schema.sql` – schéma SQLite (verze 1)

## Klíčová rozhodnutí (už odsouhlasená)
- **Stack:** Flutter (UI) + Rust (audio jádro, knihovna, analýza), propojení přes `flutter_rust_bridge`.
- **Jen lokální soubory.** Spotify a YouTube se NEintegrují (nejde je míchat do fronty s vlastním DSP).
- Hlavní Rust crate: symphonia (dekódování), cpal (výstup WASAPI/AAudio), rubato (resampling),
  rtrb (lock-free fronty), crossbeam-channel, rusqlite (bundled, FTS5), lofty (tagy),
  ebur128, signalsmith-stretch (time-stretch, MIT), rusty-chromaprint, reqwest + tokio, notify, walkdir.
- Flutter: `audio_service` + `audio_session` pro Android (foreground service, notifikace, lock screen,
  audio focus). Windows SMTC je v Rustu (crate `windows`, modul `dap_api::media_controls`).
- Databáze: SQLite ve WAL režimu, jedno zapisující vlákno, více čtecích spojení.
- Android: přístup ke složkám přes `READ_MEDIA_AUDIO` a běžné cesty, ne SAF.
- Opus se v MVP nepodporuje (symphonia ho nemá); doplní se později přes libopus.
- App ID `cz.daveprokop.daveaudioplayer`, minimální Android 8.0 (API 26).
- Bridge: flutter_rust_bridge 2.13; crate `api` se jmenuje `dap_api` (cargokit odvozuje název knihovny
  z názvu balíčku). Události do Dartu = jedna struktura `AppEvent` s `kind` (bez freezed).
- Sken knihovny běží po dávkách (`ScanJob`) jako úlohy jediného DB writer vlákna.

## Tvrdá pravidla
- **Audio vlákno (cpal callback) nesmí alokovat, zamykat ani dělat I/O.** Komunikuje jen přes lock-free fronty.
- Time-stretch a resampling běží v dekodérových vláknech decků, ne v callbacku.
- Inteligence přechodů je v plánovači (řídicí vlákno); mixer jen vyhodnocuje `TransitionPlan` (automatizační křivky).
- Na hranici FFI zachytávej panicky. Chyba dekódování = přeskočit track, nikdy pád aplikace.
- Ruční úpravy metadat (tabulka `tag_edit`) nesmí přepsat rescan souboru.
- Pozici přehrávání neposílat do Dartu každý frame – posílat čas + pozici + rychlost, Dart dopočítává.

## Vývoj (příkazy)
- Rust: `cd rust && cargo test --workspace && cargo clippy --workspace --all-targets`
- Flutter: `cd app && flutter run -d windows`, `flutter test`, `flutter test integration_test -d windows`
- Po změně `rust/crates/api/src/api/*`: `cd app && flutter_rust_bridge_codegen generate`
- Windows vyžaduje zapnutý Režim vývojáře (Flutter pluginy používají symlinky).
- Konzolové nástroje: `cargo run -p dap-engine --example player -- <db> <složka>`,
  `cargo run -p dap-library --example scan -- <db> <složka> [hledání]`

## Plánovaná struktura repozitáře
```
DaveAudioPlayer/
  CLAUDE.md
  docs/                 koncept, architektura, rozhodnutí
  db/                   schema.sql, migrace
  rust/                 Cargo workspace
    crates/audio-io  dsp  mixer  analysis  library  tags  metadata  engine  api
  app/                  Flutter aplikace
  design/               mockupy UI
```

## Pořadí vývoje
1. **MVP:** skenování složek, knihovna + hledání, fronta s drag&drop, gapless, 10pásmový EQ,
   jednoduchý crossfade s vyrovnáním hlasitosti. Ověřit řetězec Rust ↔ Flutter na Windows i Androidu.
2. Metadata (MusicBrainz, AcoustID, Cover Art Archive, LRCLIB) a jejich editace.
3. Smart Mix (analýza + typy přechodů + auto-řazení fronty).
4. Rozšiřující funkce (chytré playlisty, statistiky, scrobbling, texty, dálkové ovládání z mobilu, sync).

## Stav a další krok
Hotovo: knihovna (sken, tagy, hledání), fronta v DB, gapless (dva decky), 10pásmový EQ, bridge
do Flutteru ověřený integračním testem na Windows, ověřovací UI, ovládání médií (Windows SMTC
v Rustu ověřené testem, Android `audio_service`), crossfade s vyrovnáním hlasitosti
(EBU R128, −18 LUFS, analýza na pozadí; navazující skladby alba gapless).
Zbývá do MVP: ověření na Android zařízení (včetně přehrávání se zhasnutým displejem), obaly alb
(i do SMTC a notifikace), sledování složek (notify), návrh UI.
