//! Browsing the library by its folders on disk.

use std::collections::BTreeMap;

use anyhow::Result;

use super::library::{TrackInfo, current_covers_dir, query_track, with_reader};

/// A subfolder: its name, its path below the library folder, and how many playable
/// tracks it holds (with its own subfolders).
pub struct FolderEntry {
    pub name: String,
    pub dir: String,
    pub track_count: u32,
}

/// One folder's content. At the top (no library folder chosen) the library folders are
/// listed, each with its `root_id`.
pub struct FolderListing {
    pub folders: Vec<FolderEntry>,
    /// For the top level: the library folder of each entry in `folders`.
    pub root_ids: Vec<i64>,
    pub tracks: Vec<TrackInfo>,
}

/// `dir` with a trailing slash ("" stays ""), the prefix of everything inside it.
fn prefix(dir: &str) -> String {
    let d = dir.trim_matches('/');
    if d.is_empty() { String::new() } else { format!("{d}/") }
}

/// Files (id, path below the library folder) of a library folder inside `dir`.
fn files_in(c: &rusqlite::Connection, root_id: i64, dir: &str) -> rusqlite::Result<Vec<(i64, String)>> {
    let p = prefix(dir);
    let mut st = c.prepare_cached(
        "SELECT t.id, f.rel_path FROM file f JOIN track t ON t.file_id = f.id
         WHERE f.root_id = ?1 AND f.missing = 0 AND substr(f.rel_path, 1, length(?2)) = ?2
         ORDER BY f.rel_path COLLATE NOCASE",
    )?;
    let rows = st.query_map(rusqlite::params![root_id, p], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

/// The subfolders and tracks of `dir` in library folder `root_id`; without a library
/// folder, the library folders themselves.
pub fn folder_listing(root_id: Option<i64>, dir: String) -> Result<FolderListing> {
    let covers = current_covers_dir()?;
    with_reader(|c| {
        let Some(root_id) = root_id else {
            let mut st = c.prepare_cached(
                "SELECT r.id, r.path,
                        (SELECT count(*) FROM file f JOIN track t ON t.file_id = f.id
                         WHERE f.root_id = r.id AND f.missing = 0)
                 FROM library_root r ORDER BY r.path COLLATE NOCASE",
            )?;
            let rows: Vec<(i64, String, u32)> = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            return Ok(FolderListing {
                root_ids: rows.iter().map(|r| r.0).collect(),
                folders: rows
                    .into_iter()
                    .map(|(_, path, track_count)| FolderEntry {
                        name: path.clone(),
                        dir: String::new(),
                        track_count,
                    })
                    .collect(),
                tracks: Vec::new(),
            });
        };
        let p = prefix(&dir);
        let mut folders: BTreeMap<String, (String, u32)> = BTreeMap::new();
        let mut here = Vec::new();
        for (track_id, rel) in files_in(c, root_id, &dir)? {
            let rest = &rel[p.len()..];
            match rest.split_once('/') {
                Some((name, _)) => {
                    let e = folders
                        .entry(name.to_lowercase())
                        .or_insert_with(|| (name.to_string(), 0));
                    e.1 += 1;
                }
                None => here.push(track_id),
            }
        }
        let mut tracks = Vec::with_capacity(here.len());
        for id in here {
            if let Some(t) = query_track(c, &covers, id)? {
                tracks.push(t);
            }
        }
        Ok(FolderListing {
            folders: folders
                .into_values()
                .map(|(name, track_count)| FolderEntry {
                    dir: format!("{p}{name}"),
                    name,
                    track_count,
                })
                .collect(),
            root_ids: Vec::new(),
            tracks,
        })
    })
}

/// Every playable track in `dir` and its subfolders, in path order (to play a folder).
pub fn folder_tracks(root_id: i64, dir: String) -> Result<Vec<TrackInfo>> {
    let covers = current_covers_dir()?;
    with_reader(|c| {
        let mut out = Vec::new();
        for (id, _) in files_in(c, root_id, &dir)? {
            if let Some(t) = query_track(c, &covers, id)? {
                out.push(t);
            }
        }
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::prefix;

    #[test]
    fn prefixes() {
        assert_eq!(prefix(""), "");
        assert_eq!(prefix("Rock/"), "Rock/");
        assert_eq!(prefix("/Rock/Czech"), "Rock/Czech/");
    }
}
