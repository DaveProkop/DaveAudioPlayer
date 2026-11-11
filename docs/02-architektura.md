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

## Cargo workspace
```
crates/
  audio-io   decoder (symphonia), resampler (rubato), output (cpal), ring buffery
  dsp        biquad EQ, filtry, delay, limiter, vyhlazování parametrů
  mixer      Deck, Mixer, TransitionPlan, automatizace
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
(hovor, navigace), pauza při odpojení sluchátek, AAudio na nativních 48 kHz.
Složky přes Storage Access Framework.

**Windows:** WASAPI shared (volitelně exclusive), SMTC pro media klávesy.

**Obecně:** catch panic na hranici FFI, chyba dekódování = skip tracku.
