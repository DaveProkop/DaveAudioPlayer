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

## Tónina
`analysis.key_camelot` + `key_confidence`. Sken zapíše tóninu z tagu (TKEY / INITIALKEY:
Camelot, Open Key i názvy jako „F#m“) s jistotou 1,0 a `algo_version` 0. Analyzátor
(`ANALYSIS_ALGO` 2) při jednom dekódování změří hlasitost i chromagram (FFT 16 384, C2–C7,
jen spektrální vrcholy, každý blok normalizovaný) a tóninu odhadne korelací s profily dur/moll
(Temperley); odhad má jistotu
≤ 0,99 a tóninu z tagu nepřepíše. Kořeny naskenované dřív si tóniny z tagů načtou jednou
(stejný mechanismus jako obaly, `scan.covers_version` = 2). UI ukazuje Camelot štítky;
kompatibilní tónina = stejné číslo, ±1 se stejným písmenem, nebo stejné číslo A↔B.
Přesnost proti tóninám z tagů (266 skladeb, `cargo run --release -p dap-engine --example
key_accuracy -- <db>`): přesně 67,7 %, smíchatelně (přesně, paralelní, soused) 85,3 %.
Profily Krumhansl / Shaath / EDMA a logaritmická komprese vyšly hůř.

## Tempo a Smart Mix řazení
BPM (`analysis.bpm` + `bpm_confidence`): detekce v analyzátoru (verze 4) – spektrální tok
(FFT 1024, hop 256), autokorelace s hřebenem přes 1–4 doby a log-normální preferencí kolem
128 BPM, výsledek složený do 70–180 BPM. Tag BPM platí jen, když s ním detekce souhlasí (i
poloviční/dvojnásobné tempo) nebo si není jistá – tagy bývají špatně (90 u DnB i hip-hopu).
Měření (`--example bpm_accuracy`): bez chybných tagů 88,6 % do ±1 BPM, 8,9 % poloviční/dvojí.
Řazení fronty pro mix (`dap_analysis::mix`): cena přechodu = tónina (vzdálenost na Camelot
kolečku, změna dur/moll) + tempo (rozdíl v %, 1 bod za 3 %, poloviční/dvojí tempo blízko);
hladový průchod od hrané skladby + 2-opt. UI ukáže návrh s počtem plynulých přechodů
předtím a potom, `ReorderUpcoming` ho použije (jen skladby za hranou, ověří, že se fronta
nezměnila). Beatmatch přechody (time-stretch) zatím ne.

## Vyrovnání hlasitosti
Vlákno `analysis` (engine) dekóduje skladby a ukládá EBU R128 integrovanou hlasitost a true
peak do tabulky `analysis` (`algo_version` = 1). Přednost má fronta za aktuální skladbou; na PC
pak na pozadí zbytek knihovny; na Androidu jen skladby ve frontě, celá knihovna jen při nabíjení
(přepínač v Nastavení, stav nabíjení posílá `MainActivity` přes EventChannel). Zesílení decku =
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

## Složky knihovny
Kořeny (`library_root`) se nepřekrývají, jinak by se skladby zobrazily dvakrát
(`dap_library::roots`). Složka uvnitř existujícího kořene se nepřidá; nadřazená složka
převezme soubory vnořených kořenů (skladby si nechají id, tedy hodnocení, statistiky i místo
ve frontě). Překrývající se kořeny z dřívějška engine při startu sloučí, u duplicitního
souboru zůstane starší záznam. Odebrání složky smaže její soubory a skladby z databáze
(kaskádou), soubory na disku zůstanou. Výběr složky: Windows systémový dialog
(`file_selector`), Android nabídne složky s hudbou z MediaStore (`IS_MUSIC`, seskupené podle
složky nejvyšší úrovně na úložišti) a vlastní procházeč úložiště a SD karet.

## Sledování složek
Vlákno `folder-watch` (engine, crate `notify`: ReadDirectoryChangesW / inotify) sleduje
povolené kořeny knihovny rekurzivně. Změna audio souboru, obrázku nebo složky označí její
kořen; po 3 s klidu (nejvýš 30 s od první změny) řídicí vlákno spustí inkrementální sken jen
dotčených kořenů. Požadavky během běžícího skenu se slučují do dalšího průchodu. Při startu
aplikace proběhne sken všech kořenů (zachytí změny, když aplikace neběžela). Automatické
skeny nehlásí nedostupnou složku (odpojený disk) a UI je zobrazí jen, když se něco změnilo.
Nesledovatelné kořeny se zkouší znovu každou minutu. Android: inotify nad sdíleným úložištěm
(FUSE) nemusí hlásit změny od jiných aplikací spolehlivě – sken při startu je pojistka;
ověřit na zařízení, případně doplnit `MediaStore` ContentObserver.

## Obaly alb
Sken (`dap_library::covers`) bere obal z tagů (přední strana, jinak první obrázek), jinak
obrázek ve složce (`cover`, `folder`, `front`, `album`, `albumart` + jpg/png/webp/bmp).
Obrázek se deduplikuje podle hashe obsahu a uloží jednou do složky `covers` vedle databáze
jako JPEG ve třech velikostech: 64 px (seznamy), 256 px (přehrávač, alba) a ≤ 1024 px
(SMTC, notifikace, zamčená obrazovka). Skladba dostane vlastní `cover_id`, album první
nalezený obal. Ruční zámek pole `cover` v `tag_edit` rescan respektuje. Kořen naskenovaný
před zavedením obalů dostane obaly jednou i pro nezměněné soubory (`scan.covers_version.<id>`).

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
