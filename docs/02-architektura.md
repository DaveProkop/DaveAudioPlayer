# Architektura Rust jádra

Základní pravidlo: **audio vlákno nesmí nikdy čekat.**

## Vlákna
```
Flutter UI ──příkazy──▶ Řídicí vlákno ──▶ Workery (sken, analýza, tagy, metadata) ──▶ SQLite
                         │        │
                         ▼        ▼ (lock-free příkazy)
                 Dekodéry decků ──▶ Audio vlákno (mix, DSP) ──▶ cpal výstup (WASAPI / AAudio)
```

**Audio vlákno** – real-time callback cpal. Bere bloky PCM z ring bufferů decků, aplikuje DSP
pro každý deck (gain, EQ, filtr, echo), smíchá, master EQ + limiter, výstup. Žádné alokace,
zámky ani I/O. Komunikace jen přes `rtrb`: přijímá příkazy, posílá pozici a události
(track skončil, underrun).

**Dekodéry decků** – jedno vlákno na deck (2 aktivní + 1 pro přednačtení kvůli gapless).
symphonia → resampling na frekvenci výstupu (rubato) → během přechodu time-stretch →
ring buffer s rezervou 0,5–1 s.

**Řídicí vlákno** – drží stav přehrávače a fronty, přijímá příkazy z Flutteru (crossbeam kanál),
~30 s před koncem tracku sestaví plán přechodu, při změně fronty plán přestaví.

**Workery** – rayon pool pro CPU (sken, tagy, analýza), tokio runtime pro síť (metadata).
Analýza má prioritní frontu: nejdřív skladby ve frontě.

**SQLite** – WAL, jeden zapisující, více čtecích spojení.

## TransitionPlan
Seznam časovaných automatizačních křivek v jednotkách vzorků: hlasitost decku A/B,
low/mid/high EQ obou decků, cutoff filtru, tempo ratio, echo send. Audio vlákno je vyhodnocuje
po blocích 64 vzorků. Mixer je „hloupý“, inteligence je v plánovači. Typy přechodů =
generátory křivek. Testování offline: plán vyrenderovat do WAV bez UI.

**Stav (MVP):** `TransitionPlan` zatím nese jen crossfade (token decku, start a konec v
snímcích výstupu). Mixer v daném snímku přepne na další deck a starý doznívá jako outgoing
deck s equal-power křivkami (sin/cos). Plánovač (řídicí vlákno) plán posílá, jakmile deck
zná přesnou délku skladby (`DeckInfo::end_frame` = délka ze symphonia přepočtená resamplerem).
Navazující skladby jednoho alba (stejné album, číslo skladby +1, příp. další disk od 1)
zůstávají gapless a přebírají zesílení předchozí skladby, aby album hrálo, jak bylo zvládnuté.
Crossfade 0–12 s (výchozí 6 s), jen při automatickém přechodu, ne při ručním přeskočení.

## Vyrovnání hlasitosti
Vlákno `analysis` (engine) dekóduje skladby a ukládá EBU R128 integrovanou hlasitost a true
peak do tabulky `analysis` (`algo_version` = 1). Přednost má fronta za aktuální skladbou; na PC
pak na pozadí zbytek knihovny, na Androidu jen skladby ve frontě (baterie). Zesílení decku =
−18 LUFS − hlasitost, omezené tak, aby true peak nepřesáhl −1 dBTP (bez limiteru), rozsah
−24…+12 dB. Neanalyzovaná skladba se bere jako −14 LUFS (−4 dB). Když analýza doběhne pro
přednačtený deck, který ještě nehraje, zesílení se mu plynule upraví; hrající skladba se nemění.

## Cargo workspace
```
crates/
  audio-io   decoder (symphonia), resampler (rubato), převod kanálů, output (cpal)
             formáty MVP: MP3, FLAC, WAV, AAC/M4A, ALAC, Ogg Vorbis; Opus až později (symphonia
             ho nemá, bude potřeba binding na libopus)
  dsp        biquad EQ, filtry, delay, limiter, vyhlazování parametrů
  mixer      Deck, Mixer, PCM ring buffery (rtrb), TransitionPlan, automatizace
  analysis   BPM/beatgrid, tónina, EBU R128, energie, intro/outro
  library    skener (walkdir + notify), SQLite (rusqlite), FTS, playlisty
  tags       lofty
  metadata   MusicBrainz, AcoustID, Cover Art Archive, LRCLIB
  engine     vlákna, příkazy, události, plánovač přechodů
  api        tenká fasáda pro flutter_rust_bridge
```
`dsp`, `mixer` a `analysis` nesmí záviset na ničem platformním (testovatelnost).

## API Flutter ↔ Rust
**Příkazy (Dart → Rust):** play, pause, seek, next, prev, operace s frontou (insert, move, remove),
set_eq, set_mix_mode, scan_folders, search, edit_tags, fetch_metadata.

**Události (Rust → Dart, streamy):** PlayerState, QueueChanged, ScanProgress,
AnalysisProgress, TransitionUpcoming.

Pravidla:
- pozice: Rust pošle čas + pozici + rychlost jen při změně, Dart dopočítává
- velké seznamy stránkovaně, Flutter virtualizuje
- obaly přes cestu k souboru v cache, ne jako bajty přes most

## Platformní specifika
**Android:** engine ve stejném procesu, foreground service přes `audio_service`, audio focus
(hovor, navigace) a pauza při odpojení sluchátek přes `audio_session`, AAudio na nativních 48 kHz.
`DapAudioHandler` (app/lib/media_session.dart) jen přeposílá příkazy do Rustu a zrcadlí stav
(pozice + čas + rychlost) do `playbackState`.
Složky přes oprávnění `READ_MEDIA_AUDIO` (Android 13+, starší `READ_EXTERNAL_STORAGE`) – Rust dostává
běžné cesty, skener (`walkdir`, `notify`) funguje stejně jako na Windows. Storage Access Framework
se nepoužívá (URI místo cest by vyžadovaly procházení v Kotlinu a předávání file descriptorů).

**Windows:** WASAPI shared (volitelně exclusive). SMTC (media klávesy, přehled „Právě hraje“) řídí
přímo Rust jádro (crate `windows`, `dap_api::media_controls`): vlákno `event-hub` rozesílá
události enginu do SMTC i do Dartu, tlačítka SMTC jdou do enginu jako běžné příkazy.
`audio_service` Windows nepodporuje.

**Obecně:** catch panic na hranici FFI, chyba dekódování = skip tracku.
