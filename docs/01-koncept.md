# Koncept aplikace

## Cíl
Rychlý hudební přehrávač pro Windows a Android, který přehrává lokální hudbu ze složek
vybraných uživatelem a umí chytré DJ přechody mezi skladbami.

## Rozsah (odsouhlaseno)
- Načítání hudby ze složek vybraných uživatelem, sledování změn ve složkách.
- Playlisty (ruční i chytré podle pravidel).
- Aktuálně hraná fronta je dobře vidět, další skladby jde přetahovat (drag&drop).
- Ekvalizér (10 pásem, presety), platí pro všechno.
- Smart Mix – chytré přechody mezi skladbami (viz níže).
- Hledání: fulltext bez ohledu na diakritiku, prefixy, tolerance překlepů, filtry
  (interpret, album, žánr, rok, BPM, tónina, hodnocení).
- Automatické dotahování metadat a jejich ruční úpravy (zápis do souboru nebo jen do DB).

**Mimo rozsah:** Spotify a YouTube. Spotify nepouští zvuk přes cizí aplikaci (nejde EQ ani mix),
od 2026 navíc Developer Mode vyžaduje Premium a max. 5 uživatelů. YouTube embed nejde zpracovávat,
extrakce audia porušuje podmínky YouTube. Bez možnosti míchat je do fronty nemá integrace smysl.

## Smart Mix
Místo obyčejného crossfade se každá skladba na pozadí analyzuje a pro každou dvojici
se zvolí nejvhodnější typ přechodu.

### Analýza (jednou, ukládá se do DB; prioritně skladby ve frontě)
- BPM a beatgrid včetně taktů a frází (8/16/32 dob)
- tónina → Camelot kolečko
- hlasitost EBU R128, true peak
- energetická křivka
- skutečný začátek a konec (ořez ticha, detekce fade-outu, intro/outro)

### Typy přechodů
| Typ | Kdy | Princip |
|---|---|---|
| Gapless | po sobě jdoucí skladby ze stejného alba | žádné míchání |
| Beatmatch + bass swap | BPM blízko (±8 %) | time-stretch, srovnání na začátek fráze, prohození basů na hranici taktu |
| Filter sweep | podobná energie, jiné tempo | odcházející high-pass, příchozí se otevírá low-passem |
| Echo out | nekompatibilní tempo i tónina | poslední doba se rozplyne v delay, nový track čistě na první dobu |
| Tvrdý střih na dobu | energické žánry | střih přesně na dobu |
| Crossfade | fallback | s vyrovnáním hlasitosti |

### Další funkce Smart Mixu
- **Auto-řazení fronty** podle harmonické kompatibility, plynulého vývoje BPM a zvolené
  energetické křivky (rozjezd → vrchol → zklidnění). Uživatel návrh vidí a potvrzuje.
- **Náhled přechodu** (přehraje se ~20 s okolo mixu).
- **Ruční doladění** přechodu konkrétní dvojice (uloží se).
- **Režimy:** Album, Rádio, Párty/DJ, Chill.

## Rozšiřující funkce (odsouhlaseno)
- chytré playlisty podle pravidel (např. „hodnocení ≥ 4 a nehráno 30 dní“)
- statistiky poslechu, scrobbling Last.fm / ListenBrainz
- synchronizované texty (LRCLIB), karaoke režim s odstraněním vokálů (ONNX, offline)
- mobil jako dálkové ovládání PC přes LAN
- synchronizace playlistů mezi PC a telefonem
- import playlistu z CSV/M3U s napárováním na lokální soubory
- detekce duplicit podle zvukového otisku
- Android Auto, lock screen, notifikace, Windows media klávesy, mini přehrávač
- sleep timer, ReplayGain, exkluzivní WASAPI pro bit-perfect výstup

## Rychlost
- skenování inkrementálně na pozadí, UI se nikdy neblokuje
- virtualizované seznamy, náhledy obalů v mezipaměti
- analýza s prioritou podle fronty, na mobilu volitelně jen při nabíjení
- audio vlákno bez alokací a zámků
