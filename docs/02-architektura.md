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
nezměnila).

## Skutečný začátek a konec skladby
Analyzátor (verze 6, `dap_analysis::bounds`) měří obálku hlasitosti po 50 ms vůči hlasité
úrovni skladby (95. percentil bloků). `audio_start_ms` = první blok nad −40 dB, konec zvuku =
poslední blok nad −40 dB. Fade-out (`ends_with_fade`): od posledního kroku (0,5 s) nad −6 dB
do konce zvuku aspoň 6 s, pokles aspoň o 20 dB a z 85 % kroků bez nárůstu. `audio_end_ms`
je konec pro přechody: konec zvuku, u fade-outu bod, kde zeslábl o 20 dB. Přechod
(crossfade i beatmatch) končí v `audio_end_ms` (mixer pak odcházející deck zahodí), další
skladba s crossfadem začne od `audio_start_ms`. Navazující skladby alba a crossfade 0 se
neořezávají. Kontrola na knihovně: `--example bounds_report -- <db> [počet]`, nebo
`-- <soubor>` pro průběh hlasitosti po 5 s.

## Energie
`dap_analysis::track_energy` (0–1) = 0,6 × hlasitost (−16…−7 LUFS) + 0,4 × tempo
(80…140 BPM, neznámé = střed). Počítá se při čtení z uloženého `loudness_lufs` a `bpm`,
bez nové analýzy. Jas a hustota transientů (údernost celkově i basů) byly zkoušené na
knihovně a balady od tanečních skladeb neoddělily (`--example energy_report`). Slabina:
staré tiché mastery vycházejí méně energické. Použití: řazení fronty (cena skoku 1 bod za
0,15, max. 3, neznámá energie nic nestojí), tvrdý střih, štítek „Energie N/10“.

## Výběr typu přechodu
Při nahrání dalšího decku (`incoming_for`, nastavení `player.smart_transitions`, jen se
zapnutým crossfadem, ne navazující skladby alba) řídicí vlákno zvolí `Mix`:
- tempo do 8 % → **beatmatch** (níže),
- obě skladby energické (≥ 0,75) → **tvrdý střih** na poslední hranici 16dobé fráze
  odcházející (10 ms prolnutí), příchozí nahraná od své první doby,
- jiné tempo, kompatibilní nebo neznámá tónina → **filter sweep**: délka podle crossfadu,
  stejné výkonové křivky, odcházející přes high-pass 20 Hz → 2 kHz, příchozí přes low-pass
  250 Hz → 18 kHz (exponenciálně, koeficienty každých 32 snímků),
- jiné tempo i nekompatibilní tónina (Camelot) → **echo out**: začne na poslední hranici
  taktu, kam se vejde 5 dob; odcházející dohraje jednu dobu do echa (zpoždění 1 doba,
  zpětná vazba 0,5, high-pass 250 Hz, utichá lineárně do konce) a během 5 ms skončí;
  příchozí je nahraná od (první doba − 1 doba odcházející), takže její první doba padne
  přesně na střih,
- bez změřeného tempa → crossfade.
Mixer jen vyhodnocuje `TransitionShape`; zpožďovací linka (max 2 s) je předalokovaná.
`PlayerState.transition` hlásí UI typ a začátek příštího přechodu (pozice v odcházející
skladbě; bez plánu = `Gapless` na konci skladby). UI ukazuje odpočet a tlačítko „Poslechnout
přechod“ (seek 10 s před začátek).

## Beatmatch přechody
Analyzátor (verze 5) ukládá i mřížku dob: přesné BPM a `first_beat_ms` (fáze dob z basové
obálky na začátku a konci skladby, perioda z počtu dob mezi nimi). Když mají sousední
skladby tempo do 8 % (`player.beatmatch`, jen se zapnutým crossfadem, ne navazující skladby
alba), řídicí vlákno nahraje další deck s `StretchPlan`: hraje od své první doby, rychlostí
`rate` = perioda příchozí / perioda odcházející po dobu jedné fráze (16 dob), pak se lineárně
vrací na 1 během další fráze. Time-stretch (crate `dap-stretch`: vendorovaný Signalsmith
Stretch, MIT, vlastní C obal přes `cc`, bez bindgenu) běží v decku; výstup je zarovnaný na
snímek: stretcher se naplní (`seek`) vstupem končícím `input_latency + output_latency * rate`
za první dobou. Přechod (`TransitionShape::Beatmatch`) začíná na poslední hranici 16dobé
fráze odcházející skladby, kam se fráze ještě vejde (jinak na poslední době), trvá 16 dob:
příchozí nastupuje bez basů (sinus), v polovině se basy prohodí (30 ms, `x + (g-1)·LP150(x)`),
pak odcházející zeslabí (kosinus). Pozice deck výstupu ↔ pozice ve skladbě se přepočítává
přes plán (`Loaded::source_frame`), rychlost pro UI je aktuální `rate`. Test
`beatmatch_lands_the_incoming_beats_on_the_outgoing_grid` ověřuje kliky do ±5 ms.

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
