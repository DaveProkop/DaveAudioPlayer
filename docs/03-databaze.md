# Databáze – zdůvodnění návrhu

Schéma: `db/schema.sql` (SQLite, verze v `PRAGMA user_version`).

- **Soubor ≠ skladba.** `file` = fyzický soubor, `track` = logická skladba (umožní CUE).
  Zmizelý soubor dostane `missing = 1`; objeví-li se jinde se stejným `quick_hash`, záznam se
  přepojí. Hodnocení, statistiky a playlisty přežijí přesun souborů.
- **Úpravy metadat** v `tag_edit` (stav `db_only` / `pending_write` / `written` / `failed`).
  `track` drží platné hodnoty, záznam v `tag_edit` funguje jako zámek proti přepsání rescanem.
  Zápis do souborů dávkově na pozadí, historie umožní vrátit úpravu.
- **Analýza** rozdělená: `analysis` (malé hodnoty pro filtry a řazení) a `analysis_data`
  (bloby: beatgrid, fráze, energetická křivka, waveform). `algo_version` pro přepočet.
- **Pořadí** v playlistech a frontě jako `REAL` – přesun = jeden UPDATE, občas normalizace.
- **Fronta v DB** – po restartu se přehrávání obnoví. `origin` odliší uživatele a auto-mix.
- **Hledání:** `track_fts` (unicode61, remove_diacritics, prefixy) + záložní `track_fts_tri`
  (trigram, podřetězce a překlepy, SQLite ≥ 3.45). Oba contentless, udržuje je Rust.
- **Chytré playlisty** jako JSON pravidla, Rust je kompiluje na SQL nad povolenými poli.
- `metadata_cache` kvůli limitu MusicBrainz (1 req/s).
