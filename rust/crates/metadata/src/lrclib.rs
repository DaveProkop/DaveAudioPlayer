//! LRCLIB (lrclib.net): free synced and plain lyrics, no API key.

use serde::Deserialize;

use crate::http::{Client, MetadataError};

const BASE: &str = "https://lrclib.net/api";
/// A search hit counts only if its length is this close to the track's (seconds).
const MAX_DURATION_GAP_S: f64 = 3.0;

/// What LRCLIB has for a track.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LrclibLyrics {
    #[serde(rename = "syncedLyrics")]
    pub synced: Option<String>,
    #[serde(rename = "plainLyrics")]
    pub plain: Option<String>,
    #[serde(default)]
    pub instrumental: bool,
}

#[derive(Deserialize)]
struct Hit {
    duration: Option<f64>,
    #[serde(flatten)]
    lyrics: LrclibLyrics,
}

/// Looks up lyrics by artist, title, album and length; falls back to a search when the
/// exact lookup misses (another album name, a slightly different length).
pub fn lookup(
    client: &Client,
    artist: &str,
    title: &str,
    album: &str,
    duration_s: f64,
) -> Result<Option<LrclibLyrics>, MetadataError> {
    let duration = format!("{}", duration_s.round() as i64);
    let exact = client
        .get(format!("{BASE}/get"))
        .query(&[
            ("artist_name", artist),
            ("track_name", title),
            ("album_name", album),
            ("duration", duration.as_str()),
        ])
        .send()?;
    if exact.status().is_success() {
        return Ok(Some(exact.json()?));
    }
    if exact.status() != reqwest::StatusCode::NOT_FOUND {
        return Err(MetadataError::Http(format!("LRCLIB answered {}", exact.status())));
    }
    let hits: Vec<Hit> = client
        .get(format!("{BASE}/search"))
        .query(&[("artist_name", artist), ("track_name", title)])
        .send()?
        .error_for_status()?
        .json()?;
    Ok(best_hit(hits, duration_s))
}

/// The hit closest in length (within a few seconds), preferring synced lyrics.
fn best_hit(hits: Vec<Hit>, duration_s: f64) -> Option<LrclibLyrics> {
    hits.into_iter()
        .filter_map(|h| {
            let gap = (h.duration? - duration_s).abs();
            (gap <= MAX_DURATION_GAP_S).then_some((gap, h.lyrics))
        })
        .filter(|(_, l)| l.synced.is_some() || l.plain.is_some())
        .min_by_key(|(gap, l)| (l.synced.is_none(), (gap * 10.0) as i64))
        .map(|(_, l)| l)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_prefers_close_synced_hits() {
        let hits: Vec<Hit> = serde_json::from_str(
            r#"[
              {"duration": 200.0, "syncedLyrics": null, "plainLyrics": "plain close"},
              {"duration": 201.5, "syncedLyrics": "[00:01.00]synced", "plainLyrics": "p"},
              {"duration": 260.0, "syncedLyrics": "[00:01.00]far", "plainLyrics": "far"}
            ]"#,
        )
        .unwrap();
        let best = best_hit(hits, 200.0).unwrap();
        assert_eq!(best.synced.as_deref(), Some("[00:01.00]synced"));
        assert!(best_hit(Vec::new(), 200.0).is_none());
    }
}
