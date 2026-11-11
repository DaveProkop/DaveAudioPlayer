//! Cover art: found in tags or as an image next to the audio files, stored once per image
//! (deduplicated by content hash) as JPEG files in the covers folder:
//!
//! - `<hash>.jpg`     up to 1024 px (lock screen, media controls, large views)
//! - `<hash>_256.jpg` 256 px (album views)
//! - `<hash>_64.jpg`  64 px (lists)
//!
//! The UI gets file paths, never image bytes over the bridge.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView};
use rusqlite::{Connection, OptionalExtension, params};

use crate::db::{DbError, Result};

pub const LARGE: u32 = 1024;
pub const MEDIUM: u32 = 256;
pub const SMALL: u32 = 64;
const QUALITY: u8 = 85;

/// Image file names (lowercase stem) used for folder covers, best first.
const FOLDER_NAMES: [&str; 6] = ["cover", "folder", "front", "album", "albumart", "albumartsmall"];
const IMAGE_EXTENSIONS: [&str; 5] = ["jpg", "jpeg", "png", "webp", "bmp"];

/// The covers folder that belongs to a library database (next to it).
pub fn covers_dir(db_path: &Path) -> PathBuf {
    db_path.with_file_name("covers")
}

/// Paths of the stored sizes of one cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverFiles {
    pub small: PathBuf,
    pub medium: PathBuf,
    pub large: PathBuf,
}

/// File paths for the cover with this stored hash.
pub fn cover_files(dir: &Path, hash: &[u8]) -> CoverFiles {
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    CoverFiles {
        small: dir.join(format!("{hex}_{SMALL}.jpg")),
        medium: dir.join(format!("{hex}_{MEDIUM}.jpg")),
        large: dir.join(format!("{hex}.jpg")),
    }
}

fn content_hash(bytes: &[u8]) -> Vec<u8> {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(PRIME);
    }
    // The length too, so that the hash of a truncated copy differs.
    h ^= bytes.len() as u64;
    h.wrapping_mul(PRIME).to_be_bytes().to_vec()
}

/// Stores a cover image and returns its id; an image seen before is reused. Returns
/// `Ok(None)` for data that is not a readable image.
pub fn store_cover(conn: &Connection, dir: &Path, bytes: &[u8], source: &str) -> Result<Option<i64>> {
    let hash = content_hash(bytes);
    let files = cover_files(dir, &hash);
    let existing: Option<i64> = conn
        .query_row("SELECT id FROM cover WHERE hash = ?1", [&hash], |r| r.get(0))
        .optional()?;
    if let Some(id) = existing
        && files.small.exists()
        && files.medium.exists()
        && files.large.exists()
    {
        return Ok(Some(id));
    }

    let Ok(img) = image::load_from_memory(bytes) else {
        return Ok(None);
    };
    let (width, height) = img.dimensions();
    std::fs::create_dir_all(dir).map_err(DbError::Io)?;
    // Large first: the smaller sizes are made from it, which is much faster than from a
    // multi-megapixel original.
    let large = fit(&img, LARGE);
    write_jpeg(&large, &files.large)?;
    let medium = fit(&large, MEDIUM);
    write_jpeg(&medium, &files.medium)?;
    write_jpeg(&fit(&medium, SMALL), &files.small)?;

    let large_name = files
        .large
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let id = match existing {
        Some(id) => id, // files were deleted from the cache; now they are back
        None => {
            conn.execute(
                "INSERT INTO cover (hash, source, cache_path, width, height)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![hash, source, large_name, width, height],
            )?;
            conn.last_insert_rowid()
        }
    };
    Ok(Some(id))
}

/// Scales down to fit `max` × `max` (never up), keeping the aspect ratio.
fn fit(img: &DynamicImage, max: u32) -> DynamicImage {
    let (w, h) = img.dimensions();
    if w <= max && h <= max {
        img.clone()
    } else {
        img.resize(max, max, FilterType::Triangle)
    }
}

/// Writes atomically (temp file + rename), so a reader never sees half a JPEG.
fn write_jpeg(img: &DynamicImage, path: &Path) -> Result<()> {
    let mut buf = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut buf, QUALITY)
        .encode_image(&img.to_rgb8())
        .map_err(|e| DbError::File(format!("cover: {e}")))?;
    let tmp = path.with_extension("jpg.tmp");
    std::fs::write(&tmp, buf.into_inner()).map_err(DbError::Io)?;
    std::fs::rename(&tmp, path).map_err(DbError::Io)?;
    Ok(())
}

/// The cover image file in a folder (`cover.jpg`, `folder.png`, …), if there is one.
pub fn folder_image(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(usize, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase);
        let (Some(ext), Some(stem)) = (ext, stem) else {
            continue;
        };
        if !IMAGE_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        if let Some(rank) = FOLDER_NAMES.iter().position(|n| *n == stem)
            && best.as_ref().is_none_or(|(r, _)| rank < *r)
        {
            best = Some((rank, path));
        }
    }
    best.map(|(_, p)| p)
}

/// Folder covers already stored during one scan, by folder.
#[derive(Default)]
pub struct FolderCovers {
    by_dir: HashMap<PathBuf, Option<i64>>,
}

impl FolderCovers {
    /// The stored cover of the image in `dir`, storing it on first use.
    pub fn get(&mut self, conn: &Connection, covers: &Path, dir: &Path) -> Result<Option<i64>> {
        if let Some(id) = self.by_dir.get(dir) {
            return Ok(*id);
        }
        let id = match folder_image(dir) {
            Some(path) => match std::fs::read(&path) {
                Ok(bytes) => store_cover(conn, covers, &bytes, "folder")?,
                Err(_) => None,
            },
            None => None,
        };
        self.by_dir.insert(dir.to_path_buf(), id);
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([x as u8, y as u8, 128]));
        let mut buf = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dap-covers-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn stores_three_sizes_once_per_image() {
        let dir = temp_dir("sizes");
        let conn = open_in_memory().unwrap();
        let bytes = png(2000, 1000);
        let id = store_cover(&conn, &dir, &bytes, "embedded").unwrap().unwrap();
        assert_eq!(
            store_cover(&conn, &dir, &bytes, "folder").unwrap(),
            Some(id),
            "same image, same cover"
        );
        let hash: Vec<u8> = conn
            .query_row("SELECT hash FROM cover WHERE id = ?1", [id], |r| r.get(0))
            .unwrap();
        let files = cover_files(&dir, &hash);
        for (path, w, h) in [
            (&files.large, 1024, 512),
            (&files.medium, 256, 128),
            (&files.small, 64, 32),
        ] {
            let img = image::open(path).unwrap();
            assert_eq!(img.dimensions(), (w, h), "{}", path.display());
        }
        let count: i64 = conn
            .query_row("SELECT count(*) FROM cover", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // Cache cleared: the files come back under the same id.
        std::fs::remove_file(&files.small).unwrap();
        assert_eq!(store_cover(&conn, &dir, &bytes, "embedded").unwrap(), Some(id));
        assert!(files.small.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn small_images_are_not_scaled_up_and_garbage_is_skipped() {
        let dir = temp_dir("small");
        let conn = open_in_memory().unwrap();
        let id = store_cover(&conn, &dir, &png(100, 100), "embedded")
            .unwrap()
            .unwrap();
        let hash: Vec<u8> = conn
            .query_row("SELECT hash FROM cover WHERE id = ?1", [id], |r| r.get(0))
            .unwrap();
        assert_eq!(image::open(cover_files(&dir, &hash).large).unwrap().dimensions(), (100, 100));
        assert_eq!(store_cover(&conn, &dir, b"not an image", "embedded").unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_image_prefers_cover_over_folder_and_ignores_others() {
        let dir = temp_dir("folder");
        for name in ["Folder.JPG", "cover.png", "back.jpg", "notes.txt"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        assert_eq!(folder_image(&dir), Some(dir.join("cover.png")));
        std::fs::remove_file(dir.join("cover.png")).unwrap();
        assert_eq!(folder_image(&dir), Some(dir.join("Folder.JPG")));
        std::fs::remove_file(dir.join("Folder.JPG")).unwrap();
        assert_eq!(folder_image(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
