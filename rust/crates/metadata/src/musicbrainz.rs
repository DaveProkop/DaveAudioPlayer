//! MusicBrainz (release lookup) and the Cover Art Archive (front covers).
//!
//! MusicBrainz allows one request per second per client; every call here waits for its
//! turn on a process-wide clock.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::http::{Client, MetadataError};

const MB: &str = "https://musicbrainz.org/ws/2";
const CAA: &str = "https://coverartarchive.org";
const MB_INTERVAL: Duration = Duration::from_millis(1100);
/// Search hits below this score (0–100) are not trusted.
const MIN_SCORE: u32 = 90;

static LAST_MB_REQUEST: Mutex<Option<Instant>> = Mutex::new(None);

/// Waits until MusicBrainz may be asked again.
fn mb_turn() {
    let mut last = LAST_MB_REQUEST.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(at) = *last {
        let since = at.elapsed();
        if since < MB_INTERVAL {
            std::thread::sleep(MB_INTERVAL - since);
        }
    }
    *last = Some(Instant::now());
}

#[derive(Deserialize)]
struct ReleaseSearch {
    #[serde(default)]
    releases: Vec<Release>,
}

#[derive(Deserialize)]
struct Release {
    id: String,
    #[serde(default)]
    score: u32,
    #[serde(rename = "cover-art-archive")]
    cover_art: Option<CoverArtInfo>,
}

#[derive(Deserialize)]
struct CoverArtInfo {
    #[serde(default)]
    front: bool,
}

/// Lucene query escaping for MusicBrainz search terms.
fn quote(term: &str) -> String {
    let escaped: String = term
        .chars()
        .flat_map(|c| match c {
            '"' | '\\' => vec!['\\', c],
            _ => vec![c],
        })
        .collect();
    format!("\"{escaped}\"")
}

/// The MusicBrainz id of the best matching release that has front cover art, if a
/// confident match exists.
pub fn find_release(
    client: &Client,
    artist: &str,
    album: &str,
) -> Result<Option<String>, MetadataError> {
    let query = format!("release:{} AND artist:{}", quote(album), quote(artist));
    mb_turn();
    let found: ReleaseSearch = client
        .get(format!("{MB}/release/"))
        .query(&[("query", query.as_str()), ("fmt", "json"), ("limit", "10")])
        .send()?
        .error_for_status()?
        .json()?;
    Ok(pick_release(found.releases))
}

/// The highest scoring confident release, preferring ones known to have a front cover.
fn pick_release(releases: Vec<Release>) -> Option<String> {
    releases
        .into_iter()
        .filter(|r| r.score >= MIN_SCORE)
        .max_by_key(|r| (r.cover_art.as_ref().is_some_and(|c| c.front), r.score))
        .map(|r| r.id)
}

/// The front cover of a release (about 500 px), or `None` when it has none.
pub fn front_cover(client: &Client, release_id: &str) -> Result<Option<Vec<u8>>, MetadataError> {
    let response = client
        .get(format!("{CAA}/release/{release_id}/front-500"))
        .send()?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    Ok(Some(response.error_for_status()?.bytes()?.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_a_confident_release_with_a_cover() {
        let found: ReleaseSearch = serde_json::from_str(
            r#"{"releases": [
                {"id": "a", "score": 100, "cover-art-archive": {"front": false}},
                {"id": "b", "score": 95, "cover-art-archive": {"front": true}},
                {"id": "c", "score": 60, "cover-art-archive": {"front": true}}
            ]}"#,
        )
        .unwrap();
        assert_eq!(pick_release(found.releases).as_deref(), Some("b"));
        let weak: ReleaseSearch =
            serde_json::from_str(r#"{"releases": [{"id": "c", "score": 60}]}"#).unwrap();
        assert_eq!(pick_release(weak.releases), None);
    }

    #[test]
    fn search_terms_are_quoted() {
        assert_eq!(quote(r#"Say "Hi""#), r#""Say \"Hi\"""#);
    }
}
