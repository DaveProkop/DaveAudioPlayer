-- =====================================================================
-- Hudební přehrávač – schéma SQLite (verze 1)
-- Spouštět při každém otevření spojení:
--   PRAGMA journal_mode = WAL;
--   PRAGMA foreign_keys = ON;
--   PRAGMA synchronous = NORMAL;
-- Verze schématu se drží v PRAGMA user_version (migrace v Rustu).
-- =====================================================================

PRAGMA user_version = 1;

-- ---------------------------------------------------------------------
-- 1. Soubory a složky
-- ---------------------------------------------------------------------

-- Složky, které vybral uživatel
CREATE TABLE library_root (
    id          INTEGER PRIMARY KEY,
    path        TEXT    NOT NULL UNIQUE,      -- absolutní cesta (Windows i Android)
    enabled     INTEGER NOT NULL DEFAULT 1,
    watch       INTEGER NOT NULL DEFAULT 1,   -- sledovat změny (notify)
    added_at    INTEGER NOT NULL              -- unix ms
);

-- Fyzický soubor na disku
CREATE TABLE file (
    id           INTEGER PRIMARY KEY,
    root_id      INTEGER NOT NULL REFERENCES library_root(id) ON DELETE CASCADE,
    rel_path     TEXT    NOT NULL,
    size         INTEGER NOT NULL,
    mtime        INTEGER NOT NULL,
    quick_hash   BLOB,                        -- FNV-1a 64 z velikosti + prvních/posledních 16 kB → detekce přesunu
    format       TEXT    NOT NULL,            -- mp3, flac, m4a, ogg, opus, wav…
    duration_ms  INTEGER,
    sample_rate  INTEGER,
    channels     INTEGER,
    bit_depth    INTEGER,
    bitrate      INTEGER,
    missing      INTEGER NOT NULL DEFAULT 0,  -- soubor zmizel, data se drží kvůli přesunu
    scanned_at   INTEGER NOT NULL,
    UNIQUE (root_id, rel_path)
);
CREATE INDEX ix_file_hash ON file(quick_hash) WHERE missing = 1;

-- ---------------------------------------------------------------------
-- 2. Katalog
-- ---------------------------------------------------------------------

CREATE TABLE artist (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    name_sort   TEXT NOT NULL,                -- "Beatles, The"
    mbid        TEXT UNIQUE                   -- MusicBrainz artist ID
);
CREATE INDEX ix_artist_sort ON artist(name_sort);

CREATE TABLE cover (
    id          INTEGER PRIMARY KEY,
    hash        BLOB    NOT NULL UNIQUE,      -- deduplikace stejných obalů
    source      TEXT    NOT NULL,             -- embedded | folder | coverartarchive | user
    cache_path  TEXT    NOT NULL,             -- originál v mezipaměti
    width       INTEGER,
    height      INTEGER
);                                            -- náhledy: <cache>/<hash>_{64,256}.webp

CREATE TABLE album (
    id               INTEGER PRIMARY KEY,
    title            TEXT NOT NULL,
    title_sort       TEXT NOT NULL,
    album_artist_id  INTEGER REFERENCES artist(id),
    year             INTEGER,
    total_discs      INTEGER,
    cover_id         INTEGER REFERENCES cover(id),
    mbid             TEXT UNIQUE,             -- MusicBrainz release ID
    is_compilation   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX ix_album_sort ON album(title_sort);

-- Skladba = logická jednotka; obvykle 1:1 se souborem, u CUE více skladeb v souboru
CREATE TABLE track (
    id            INTEGER PRIMARY KEY,
    file_id       INTEGER NOT NULL REFERENCES file(id) ON DELETE CASCADE,
    start_ms      INTEGER NOT NULL DEFAULT 0,  -- pro CUE
    end_ms        INTEGER,
    title         TEXT    NOT NULL,
    title_sort    TEXT    NOT NULL,
    album_id      INTEGER REFERENCES album(id),
    track_no      INTEGER,
    disc_no       INTEGER,
    year          INTEGER,
    comment       TEXT,
    cover_id      INTEGER REFERENCES cover(id),  -- vlastní obal, jinak z alba
    mbid          TEXT,                          -- MusicBrainz recording ID
    acoustid      TEXT,
    -- uživatelská data (přežijí rescan i přesun souboru)
    rating        INTEGER CHECK (rating BETWEEN 0 AND 10),
    play_count    INTEGER NOT NULL DEFAULT 0,
    skip_count    INTEGER NOT NULL DEFAULT 0,
    last_played   INTEGER,
    added_at      INTEGER NOT NULL,
    UNIQUE (file_id, start_ms)
);
CREATE INDEX ix_track_album      ON track(album_id, disc_no, track_no);
CREATE INDEX ix_track_lastplayed ON track(last_played);
CREATE INDEX ix_track_added      ON track(added_at);

CREATE TABLE track_artist (
    track_id   INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    artist_id  INTEGER NOT NULL REFERENCES artist(id),
    role       TEXT    NOT NULL DEFAULT 'main',  -- main | feat | remixer | composer
    position   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (track_id, artist_id, role)
);
CREATE INDEX ix_track_artist_artist ON track_artist(artist_id);

CREATE TABLE genre (
    id    INTEGER PRIMARY KEY,
    name  TEXT NOT NULL UNIQUE COLLATE NOCASE
);
CREATE TABLE track_genre (
    track_id  INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    genre_id  INTEGER NOT NULL REFERENCES genre(id),
    PRIMARY KEY (track_id, genre_id)
);
CREATE INDEX ix_track_genre_genre ON track_genre(genre_id);

CREATE TABLE lyrics (
    track_id  INTEGER PRIMARY KEY REFERENCES track(id) ON DELETE CASCADE,
    synced    INTEGER NOT NULL,              -- 1 = LRC s časy
    body      TEXT    NOT NULL,
    source    TEXT    NOT NULL               -- embedded | lrc_file | lrclib | user
);

-- ---------------------------------------------------------------------
-- 3. Úpravy metadat
-- ---------------------------------------------------------------------

-- Každá ruční nebo online úprava pole. Jakýkoli záznam pro (track_id, field)
-- zároveň funguje jako zámek: rescan souboru pole nepřepíše.
CREATE TABLE tag_edit (
    id          INTEGER PRIMARY KEY,
    track_id    INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    field       TEXT    NOT NULL,            -- title, album, artist, year, genre, cover…
    old_value   TEXT,
    new_value   TEXT,
    source      TEXT    NOT NULL,            -- user | musicbrainz | acoustid | lastfm
    status      TEXT    NOT NULL,            -- db_only | pending_write | written | failed
    error       TEXT,
    created_at  INTEGER NOT NULL
);
CREATE INDEX ix_tag_edit_track   ON tag_edit(track_id, field);
CREATE INDEX ix_tag_edit_pending ON tag_edit(status) WHERE status = 'pending_write';

-- Cache odpovědí online služeb (MusicBrainz má limit 1 req/s)
CREATE TABLE metadata_cache (
    provider    TEXT    NOT NULL,
    lookup_key  TEXT    NOT NULL,
    response    TEXT    NOT NULL,            -- JSON
    fetched_at  INTEGER NOT NULL,
    PRIMARY KEY (provider, lookup_key)
);

-- ---------------------------------------------------------------------
-- 4. Analýza pro Smart Mix
-- ---------------------------------------------------------------------

-- Malé, často dotazované hodnoty (filtry, auto-řazení)
CREATE TABLE analysis (
    track_id        INTEGER PRIMARY KEY REFERENCES track(id) ON DELETE CASCADE,
    algo_version    INTEGER NOT NULL,        -- při změně algoritmu se přepočítá
    bpm             REAL,
    bpm_confidence  REAL,
    key_camelot     TEXT,                    -- '8A', '11B'…
    key_confidence  REAL,
    loudness_lufs   REAL,                    -- EBU R128 integrated
    true_peak_db    REAL,
    energy          REAL,                    -- průměrná energie 0–1
    first_beat_ms   INTEGER,
    intro_end_ms    INTEGER,
    outro_start_ms  INTEGER,
    audio_start_ms  INTEGER,                 -- po ořezu ticha
    audio_end_ms    INTEGER,
    ends_with_fade  INTEGER,
    analyzed_at     INTEGER NOT NULL
);
CREATE INDEX ix_analysis_bpm ON analysis(bpm);
CREATE INDEX ix_analysis_key ON analysis(key_camelot);

-- Velká data odděleně, aby hlavní tabulka zůstala malá a rychlá
CREATE TABLE analysis_data (
    track_id      INTEGER PRIMARY KEY REFERENCES track(id) ON DELETE CASCADE,
    beatgrid      BLOB,                      -- pozice dob/taktů (delta-kódované i32)
    phrases       BLOB,                      -- hranice frází 8/16/32 taktů
    energy_curve  BLOB,                      -- energie po 0,5 s (u8)
    waveform      BLOB                       -- náhled vlny pro UI (u8, ~1000 bodů)
);

-- Ručně doladěné přechody pro konkrétní dvojici skladeb
CREATE TABLE transition_override (
    from_track_id  INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    to_track_id    INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    kind           TEXT    NOT NULL,         -- gapless | beatmatch | filter | echo | cut | crossfade
    params         TEXT    NOT NULL,         -- JSON: body mixu, délka, křivky
    updated_at     INTEGER NOT NULL,
    PRIMARY KEY (from_track_id, to_track_id)
);

-- ---------------------------------------------------------------------
-- 5. Playlisty a fronta
-- ---------------------------------------------------------------------

CREATE TABLE playlist (
    id          INTEGER PRIMARY KEY,
    name        TEXT    NOT NULL,
    kind        TEXT    NOT NULL DEFAULT 'manual',  -- manual | smart
    rules       TEXT,                    -- JSON pravidla chytrého playlistu (kompiluje Rust)
    mix_mode    TEXT,                    -- výchozí režim přechodů pro playlist
    cover_id    INTEGER REFERENCES cover(id),
    sort_order  REAL    NOT NULL,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

-- Pořadí jako REAL: přesun = nová hodnota mezi sousedy, bez přečíslování
CREATE TABLE playlist_item (
    id           INTEGER PRIMARY KEY,
    playlist_id  INTEGER NOT NULL REFERENCES playlist(id) ON DELETE CASCADE,
    track_id     INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    pos          REAL    NOT NULL,
    added_at     INTEGER NOT NULL
);
CREATE INDEX ix_playlist_item ON playlist_item(playlist_id, pos);

-- Aktuální fronta (přežije restart aplikace)
CREATE TABLE queue_item (
    id                  INTEGER PRIMARY KEY,
    track_id            INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    pos                 REAL    NOT NULL,
    origin              TEXT    NOT NULL,     -- user | playlist | automix
    origin_playlist_id  INTEGER REFERENCES playlist(id) ON DELETE SET NULL
);
CREATE INDEX ix_queue_pos ON queue_item(pos);

-- ---------------------------------------------------------------------
-- 6. Historie a statistiky
-- ---------------------------------------------------------------------

CREATE TABLE play_history (
    id               INTEGER PRIMARY KEY,
    track_id         INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    started_at       INTEGER NOT NULL,
    played_ms        INTEGER NOT NULL,
    completed        INTEGER NOT NULL,       -- dohráno (≥ 50 % / 4 min)
    skipped          INTEGER NOT NULL,
    transition_kind  TEXT,                   -- jakým přechodem skladba začala
    device           TEXT                    -- pc | android (pro sync)
);
CREATE INDEX ix_history_track ON play_history(track_id, started_at);
CREATE INDEX ix_history_time  ON play_history(started_at);

-- Fronta k odeslání na Last.fm / ListenBrainz (offline-friendly)
CREATE TABLE scrobble (
    history_id  INTEGER NOT NULL REFERENCES play_history(id) ON DELETE CASCADE,
    service     TEXT    NOT NULL,            -- lastfm | listenbrainz
    status      TEXT    NOT NULL,            -- pending | sent | failed
    attempts    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (history_id, service)
);

-- ---------------------------------------------------------------------
-- 7. Fulltext hledání
-- ---------------------------------------------------------------------

-- Hlavní index: slova, bez diakritiky, prefixové hledání ("prag" → "Praha")
-- rowid = track.id; obsah udržuje Rust při každé změně skladby
CREATE VIRTUAL TABLE track_fts USING fts5(
    title, artists, album, genres, lyrics,
    content = '',
    contentless_delete = 1,
    tokenize = 'unicode61 remove_diacritics 2',
    prefix = '2 3'
);

-- Záložní index pro podřetězce a překlepy (SQLite ≥ 3.45)
CREATE VIRTUAL TABLE track_fts_tri USING fts5(
    title, artists, album,
    content = '',
    contentless_delete = 1,
    tokenize = 'trigram remove_diacritics 1'
);

-- ---------------------------------------------------------------------
-- 8. Nastavení a stav
-- ---------------------------------------------------------------------

CREATE TABLE setting (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL                     -- JSON hodnota
);
-- příklady klíčů: eq.preset, eq.bands, mix.mode, player.current_queue_item,
-- player.position_ms, analysis.on_charging_only, scan.excluded_patterns
