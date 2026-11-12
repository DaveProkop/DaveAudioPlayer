//! Reading and writing audio file tags (lofty).
//!
//! Reading merges all tags in a file (e.g. ID3v2 + APE): for each field the primary tag
//! wins, other tags fill gaps.

use std::fmt;
use std::path::Path;

use lofty::config::ParseOptions;
use lofty::picture::PictureType;
use lofty::prelude::{Accessor, AudioFile, ItemKey, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::Tag;

/// Logical metadata of a track as found in the file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackTags {
    pub title: Option<String>,
    /// Track artists in order (multi-value tags and `;` / ` / ` lists are split).
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track_no: Option<u32>,
    pub track_total: Option<u32>,
    pub disc_no: Option<u32>,
    pub disc_total: Option<u32>,
    pub year: Option<i32>,
    pub genres: Vec<String>,
    pub comment: Option<String>,
    pub lyrics: Option<String>,
    pub compilation: bool,
    pub mb_recording_id: Option<String>,
    pub mb_release_id: Option<String>,
    pub acoustid: Option<String>,
    /// Musical key as written by DJ software ("8A", "Am", "1m"…), unparsed.
    pub initial_key: Option<String>,
    pub has_embedded_cover: bool,
}

/// Technical properties of the audio stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioProperties {
    pub duration_ms: u64,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    pub bit_depth: Option<u8>,
    pub bitrate_kbps: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileTags {
    pub tags: TrackTags,
    pub properties: AudioProperties,
    /// Embedded cover image bytes (the front cover if marked, else the first picture).
    pub cover: Option<Vec<u8>>,
}

#[derive(Debug)]
pub enum TagError {
    Io(std::io::Error),
    /// The file could not be parsed (unknown format or corrupt).
    Parse(String),
}

impl fmt::Display for TagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TagError::Io(e) => write!(f, "I/O error: {e}"),
            TagError::Parse(e) => write!(f, "cannot read tags: {e}"),
        }
    }
}

impl std::error::Error for TagError {}

/// Reads tags, audio properties and the embedded cover image.
pub fn read(path: &Path) -> Result<FileTags, TagError> {
    let file = std::fs::File::open(path).map_err(TagError::Io)?;
    let probe = Probe::new(std::io::BufReader::new(file)).options(ParseOptions::new());
    let probe = probe.guess_file_type().map_err(TagError::Io)?;
    let file = probe.read().map_err(|e| TagError::Parse(e.to_string()))?;

    let p = file.properties();
    let properties = AudioProperties {
        duration_ms: p.duration().as_millis() as u64,
        sample_rate: p.sample_rate(),
        channels: p.channels(),
        bit_depth: p.bit_depth(),
        bitrate_kbps: p.audio_bitrate().or(p.overall_bitrate()),
    };

    // Primary tag first, then the rest.
    let mut tags: Vec<&Tag> = Vec::new();
    if let Some(primary) = file.primary_tag() {
        tags.push(primary);
    }
    for t in file.tags() {
        if !tags.iter().any(|x| std::ptr::eq(*x, t)) {
            tags.push(t);
        }
    }

    let pictures = || tags.iter().flat_map(|t| t.pictures());
    let cover = pictures()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or_else(|| pictures().next())
        .map(|p| p.data().to_vec())
        .filter(|d| !d.is_empty());

    Ok(FileTags {
        tags: merge(&tags),
        properties,
        cover,
    })
}

fn merge(tags: &[&Tag]) -> TrackTags {
    let text = |key: ItemKey| tags.iter().find_map(|t| clean(t.get_string(key)));
    let number = |f: fn(&Tag) -> Option<u32>| tags.iter().find_map(|t| f(t).filter(|&n| n > 0));

    let artists = tags
        .iter()
        .map(|t| {
            let multi: Vec<&str> = t.get_strings(ItemKey::TrackArtists).collect();
            if multi.is_empty() {
                split_list(t.get_strings(ItemKey::TrackArtist), ARTIST_SEPARATORS)
            } else {
                split_list(multi.into_iter(), ARTIST_SEPARATORS)
            }
        })
        .find(|a| !a.is_empty())
        .unwrap_or_default();

    let genres = tags
        .iter()
        .map(|t| split_list(t.get_strings(ItemKey::Genre), GENRE_SEPARATORS))
        .find(|g| !g.is_empty())
        .unwrap_or_default();

    let year = tags.iter().find_map(|t| {
        t.date()
            .map(|d| i32::from(d.year))
            .filter(|&y| y > 0)
            .or_else(|| parse_year(t.get_string(ItemKey::Year)))
            .or_else(|| parse_year(t.get_string(ItemKey::RecordingDate)))
            .or_else(|| parse_year(t.get_string(ItemKey::ReleaseDate)))
    });

    let compilation = tags.iter().any(|t| {
        t.get_string(ItemKey::FlagCompilation)
            .is_some_and(|v| matches!(v.trim(), "1" | "true" | "True" | "TRUE"))
    });

    TrackTags {
        title: tags.iter().find_map(|t| clean(t.title().as_deref())),
        artists,
        album: tags.iter().find_map(|t| clean(t.album().as_deref())),
        album_artist: text(ItemKey::AlbumArtist),
        track_no: number(|t| t.track()),
        track_total: number(|t| t.track_total()),
        disc_no: number(|t| t.disk()),
        disc_total: number(|t| t.disk_total()),
        year,
        genres,
        comment: tags.iter().find_map(|t| clean(t.comment().as_deref())),
        lyrics: text(ItemKey::Lyrics).or_else(|| text(ItemKey::UnsyncLyrics)),
        compilation,
        mb_recording_id: text(ItemKey::MusicBrainzRecordingId),
        mb_release_id: text(ItemKey::MusicBrainzReleaseId),
        acoustid: text(ItemKey::AcoustId),
        initial_key: text(ItemKey::InitialKey),
        has_embedded_cover: tags.iter().any(|t| !t.pictures().is_empty()),
    }
}

/// Splits on separators that are unambiguous lists. `&`, `,` and "feat." are left alone:
/// they are part of many artist names ("Simon & Garfunkel").
const ARTIST_SEPARATORS: &[&str] = &["\0", ";", " / "];
const GENRE_SEPARATORS: &[&str] = &["\0", ";", "/", ","];

fn split_list<'a>(values: impl Iterator<Item = &'a str>, separators: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for value in values {
        let mut parts = vec![value.to_owned()];
        for sep in separators {
            parts = parts
                .iter()
                .flat_map(|p| p.split(sep).map(str::to_owned))
                .collect();
        }
        for p in parts {
            let p = p.trim();
            if !p.is_empty() && !out.iter().any(|x| x.eq_ignore_ascii_case(p)) {
                out.push(p.to_owned());
            }
        }
    }
    out
}

fn clean(value: Option<&str>) -> Option<String> {
    let v = value?.trim().trim_matches('\0').trim();
    (!v.is_empty()).then(|| v.to_owned())
}

/// Takes the leading 4-digit year from values like "1999", "1999-05-01" or "1999/2000".
fn parse_year(value: Option<&str>) -> Option<i32> {
    let v = value?.trim();
    let digits: String = v.chars().take_while(char::is_ascii_digit).collect();
    if digits.len() == 4 {
        digits.parse().ok().filter(|&y| y > 0)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lofty::config::WriteOptions;
    use lofty::prelude::TagExt;
    use lofty::tag::TagType;
    use std::io::Write;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("dap-tags-test-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Writes one second of silent 16-bit stereo PCM at 44.1 kHz.
    fn write_wav(path: &Path) {
        let (rate, ch, frames) = (44_100u32, 2u16, 44_100u32);
        let data_len = frames * u32::from(ch) * 2;
        let mut f = std::fs::File::create(path).unwrap();
        let mut w = |b: &[u8]| f.write_all(b).unwrap();
        w(b"RIFF");
        w(&(36 + data_len).to_le_bytes());
        w(b"WAVEfmt ");
        w(&16u32.to_le_bytes());
        w(&1u16.to_le_bytes());
        w(&ch.to_le_bytes());
        w(&rate.to_le_bytes());
        w(&(rate * u32::from(ch) * 2).to_le_bytes());
        w(&(ch * 2).to_le_bytes());
        w(&16u16.to_le_bytes());
        w(b"data");
        w(&data_len.to_le_bytes());
        w(&vec![0u8; data_len as usize]);
    }

    fn tagged_wav(dir: &TempDir, fill: impl FnOnce(&mut Tag)) -> PathBuf {
        let path = dir.0.join("t.wav");
        write_wav(&path);
        let mut tag = Tag::new(TagType::Id3v2);
        fill(&mut tag);
        tag.save_to_path(&path, WriteOptions::default()).unwrap();
        path
    }

    #[test]
    fn reads_common_fields_and_properties() {
        let dir = TempDir::new("common");
        let path = tagged_wav(&dir, |t| {
            t.set_title("Má vlast".into());
            t.set_artist("Bedřich Smetana; Czech Philharmonic".into());
            t.set_album("Má vlast".into());
            t.insert_text(ItemKey::AlbumArtist, "Bedřich Smetana".into());
            t.set_track(2);
            t.set_track_total(6);
            t.set_disk(1);
            t.set_genre("Classical; Romantic".into());
            t.insert_text(ItemKey::RecordingDate, "1990-05-12".into());
            t.set_comment("live".into());
        });

        let f = read(&path).unwrap();
        let t = &f.tags;
        assert_eq!(t.title.as_deref(), Some("Má vlast"));
        assert_eq!(t.artists, ["Bedřich Smetana", "Czech Philharmonic"]);
        assert_eq!(t.album_artist.as_deref(), Some("Bedřich Smetana"));
        assert_eq!(
            (t.track_no, t.track_total, t.disc_no),
            (Some(2), Some(6), Some(1))
        );
        assert_eq!(t.genres, ["Classical", "Romantic"]);
        assert_eq!(t.year, Some(1990));
        assert_eq!(t.comment.as_deref(), Some("live"));
        assert!(!t.compilation);

        let p = &f.properties;
        assert_eq!(p.sample_rate, Some(44_100));
        assert_eq!(p.channels, Some(2));
        assert!((990..=1010).contains(&p.duration_ms), "{}", p.duration_ms);
    }

    #[test]
    fn embedded_front_cover_wins_over_other_pictures() {
        use lofty::picture::{MimeType, Picture};
        let dir = TempDir::new("cover");
        let path = tagged_wav(&dir, |t| {
            t.set_title("x".into());
            for (kind, byte) in [(PictureType::CoverBack, 1u8), (PictureType::CoverFront, 2)] {
                t.push_picture(
                    Picture::unchecked(vec![byte; 16])
                        .pic_type(kind)
                        .mime_type(MimeType::Png)
                        .build(),
                );
            }
        });
        assert_eq!(read(&path).unwrap().cover, Some(vec![2u8; 16]));
    }

    #[test]
    fn untagged_file_has_properties_only() {
        let dir = TempDir::new("untagged");
        let path = dir.0.join("plain.wav");
        write_wav(&path);
        let f = read(&path).unwrap();
        assert_eq!(f.tags, TrackTags::default());
        assert_eq!(f.properties.channels, Some(2));
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let dir = TempDir::new("garbage");
        let path = dir.0.join("x.mp3");
        std::fs::write(&path, b"definitely not audio").unwrap();
        assert!(read(&path).is_err());
        assert!(matches!(
            read(&dir.0.join("missing.mp3")),
            Err(TagError::Io(_))
        ));
    }

    #[test]
    fn list_splitting_keeps_band_names_intact() {
        let split = |s: &str| split_list(std::iter::once(s), ARTIST_SEPARATORS);
        assert_eq!(split("Simon & Garfunkel"), ["Simon & Garfunkel"]);
        assert_eq!(split("A; B ;a"), ["A", "B"]);
        assert_eq!(split("AC/DC"), ["AC/DC"]);
        assert_eq!(split("X / Y"), ["X", "Y"]);
    }

    #[test]
    fn year_parsing() {
        assert_eq!(parse_year(Some("1999-05-01")), Some(1999));
        assert_eq!(parse_year(Some("99")), None);
        assert_eq!(parse_year(Some("")), None);
    }
}
