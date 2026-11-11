//! Track search: word/prefix search without diacritics, falling back to substring
//! (trigram) search when nothing matches.

use rusqlite::{Connection, params};

use crate::db::Result;

/// Returns ids of present (not missing) tracks matching `query`, best matches first.
pub fn search_tracks(conn: &Connection, query: &str, limit: usize) -> Result<Vec<i64>> {
    let words = tokens(query);
    if words.is_empty() {
        return Ok(Vec::new());
    }

    // Every word must match, each as a prefix: "smet vlas" finds "Smetana – Má vlast".
    let fts_query = words
        .iter()
        .map(|w| format!("\"{w}\"*"))
        .collect::<Vec<_>>()
        .join(" ");
    let hits = run(
        conn,
        "SELECT f.rowid FROM track_fts f
         JOIN track t ON t.id = f.rowid
         JOIN file fl ON fl.id = t.file_id AND fl.missing = 0
         WHERE track_fts MATCH ?1
         ORDER BY bm25(track_fts, 10.0, 5.0, 3.0, 1.0, 0.5)
         LIMIT ?2",
        &fts_query,
        limit,
    )?;
    if !hits.is_empty() {
        return Ok(hits);
    }

    // Substrings inside words ("ziva" in "neživá"); trigrams need 3+ characters per term.
    let tri: Vec<&String> = words.iter().filter(|w| w.chars().count() >= 3).collect();
    if tri.is_empty() {
        return Ok(Vec::new());
    }
    let tri_query = tri
        .iter()
        .map(|w| format!("\"{w}\""))
        .collect::<Vec<_>>()
        .join(" ");
    run(
        conn,
        "SELECT f.rowid FROM track_fts_tri f
         JOIN track t ON t.id = f.rowid
         JOIN file fl ON fl.id = t.file_id AND fl.missing = 0
         WHERE track_fts_tri MATCH ?1
         ORDER BY rank
         LIMIT ?2",
        &tri_query,
        limit,
    )
}

fn run(conn: &Connection, sql: &str, q: &str, limit: usize) -> Result<Vec<i64>> {
    let mut st = conn.prepare_cached(sql)?;
    let rows = st.query_map(params![q, limit as i64], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Splits user input into plain words like the index tokenizer, dropping FTS5 syntax.
fn tokens(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_strips_fts_syntax() {
        assert_eq!(
            tokens("  AC/DC \"back\" in* -black"),
            ["AC", "DC", "back", "in", "black"]
        );
        // Same split as the unicode61 tokenizer.
        assert_eq!(tokens("don't"), ["don", "t"]);
        assert!(tokens("*:^()").is_empty());
    }
}
