//! ListenBrainz scrobbling: submits finished plays with the user's token.

use serde::Deserialize;
use serde_json::json;

use crate::http::{Client, MetadataError};

const API: &str = "https://api.listenbrainz.org/1";

/// One finished play.
#[derive(Debug, Clone, PartialEq)]
pub struct Listen {
    /// Unix seconds when the play started.
    pub listened_at: i64,
    pub artist: String,
    pub title: String,
    pub album: Option<String>,
    pub duration_ms: Option<i64>,
}

#[derive(Deserialize)]
struct Validation {
    valid: bool,
    user_name: Option<String>,
}

/// The user name a token belongs to, or `None` when ListenBrainz rejects it.
pub fn validate_token(client: &Client, token: &str) -> Result<Option<String>, MetadataError> {
    let v: Validation = client
        .get(format!("{API}/validate-token"))
        .header("Authorization", format!("Token {}", token.trim()))
        .send()?
        .error_for_status()?
        .json()?;
    Ok(if v.valid { v.user_name } else { None })
}

fn payload(listens: &[Listen]) -> serde_json::Value {
    let items: Vec<_> = listens
        .iter()
        .map(|l| {
            let mut meta = json!({
                "artist_name": l.artist,
                "track_name": l.title,
                "additional_info": {
                    "submission_client": "DaveAudioPlayer",
                    "submission_client_version": env!("CARGO_PKG_VERSION"),
                },
            });
            if let Some(album) = l.album.as_ref().filter(|a| !a.is_empty()) {
                meta["release_name"] = json!(album);
            }
            if let Some(ms) = l.duration_ms.filter(|ms| *ms > 0) {
                meta["additional_info"]["duration_ms"] = json!(ms);
            }
            json!({ "listened_at": l.listened_at, "track_metadata": meta })
        })
        .collect();
    json!({
        "listen_type": if listens.len() == 1 { "single" } else { "import" },
        "payload": items,
    })
}

/// Submits finished plays (at most a few hundred at once).
pub fn submit(client: &Client, token: &str, listens: &[Listen]) -> Result<(), MetadataError> {
    if listens.is_empty() {
        return Ok(());
    }
    client
        .post(format!("{API}/submit-listens"))
        .header("Authorization", format!("Token {}", token.trim()))
        .json(&payload(listens))
        .send()?
        .error_for_status()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_follows_the_api() {
        let one = Listen {
            listened_at: 1_700_000_000,
            artist: "Kabát".into(),
            title: "Pohoda".into(),
            album: Some("Colorado".into()),
            duration_ms: Some(215_000),
        };
        let p = payload(std::slice::from_ref(&one));
        assert_eq!(p["listen_type"], "single");
        assert_eq!(p["payload"][0]["listened_at"], 1_700_000_000);
        assert_eq!(p["payload"][0]["track_metadata"]["release_name"], "Colorado");
        assert_eq!(p["payload"][0]["track_metadata"]["additional_info"]["duration_ms"], 215_000);
        let two = payload(&[one.clone(), Listen { album: None, ..one }]);
        assert_eq!(two["listen_type"], "import");
        assert!(two["payload"][1]["track_metadata"].get("release_name").is_none());
    }
}
