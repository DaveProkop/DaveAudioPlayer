//! The HTTP client every online lookup uses: an identifying User-Agent (the services ask
//! for one) and short timeouts, so a slow service never stalls the app.

use std::time::Duration;

pub use reqwest::blocking::Client;

/// Sent with every request, as MusicBrainz and LRCLIB ask apps to.
pub const USER_AGENT: &str = concat!(
    "DaveAudioPlayer/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/DaveProkop/DaveAudioPlayer )"
);

#[derive(Debug)]
pub enum MetadataError {
    Http(String),
    Parse(String),
}

impl std::fmt::Display for MetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetadataError::Http(e) => write!(f, "network: {e}"),
            MetadataError::Parse(e) => write!(f, "unexpected response: {e}"),
        }
    }
}

impl std::error::Error for MetadataError {}

impl From<reqwest::Error> for MetadataError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_decode() {
            MetadataError::Parse(e.to_string())
        } else {
            MetadataError::Http(e.to_string())
        }
    }
}

pub fn client() -> Result<Client, MetadataError> {
    Ok(Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()?)
}
