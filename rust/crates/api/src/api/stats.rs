//! Listening statistics (from `play_history`) and track ratings.

use anyhow::Result;
use dap_library::catalog::now_ms;
use rusqlite::params;

use super::library::{TrackInfo, current_covers_dir, query_track, with_reader};
use super::player::write;

const DAY_MS: i64 = 24 * 3600 * 1000;
const TOP: i64 = 10;

pub struct TopTrack {
    pub track: TrackInfo,
    pub count: u32,
}

pub struct TopItem {
    pub name: String,
    pub count: u32,
}

pub struct RecentPlay {
    pub track: TrackInfo,
    /// Unix ms.
    pub at_ms: i64,
    pub skipped: bool,
}

pub struct ListenStats {
    /// Completed plays.
    pub plays: u32,
    pub skips: u32,
    pub listened_ms: i64,
    pub distinct_tracks: u32,
    pub top_tracks: Vec<TopTrack>,
    pub top_artists: Vec<TopItem>,
    pub top_genres: Vec<TopItem>,
    pub most_skipped: Vec<TopTrack>,
    pub recent: Vec<RecentPlay>,
    /// Completed plays per day, oldest first (the period's days, at most 60).
    pub per_day: Vec<u32>,
}

/// Statistics of the last `days` days (0 = all time).
pub fn listen_stats(days: u32) -> Result<ListenStats> {
    let covers = current_covers_dir()?;
    let now = now_ms();
    let since = if days == 0 { 0 } else { now - i64::from(days) * DAY_MS };
    with_reader(|c| {
        let (plays, skips, listened_ms, distinct_tracks) = c.query_row(
            "SELECT coalesce(sum(completed), 0), coalesce(sum(skipped), 0),
                    coalesce(sum(played_ms), 0), count(DISTINCT track_id)
             FROM play_history WHERE started_at >= ?1",
            [since],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let tracks = |sql: &str| -> rusqlite::Result<Vec<TopTrack>> {
            let mut st = c.prepare_cached(sql)?;
            let rows: Vec<(i64, u32)> = st
                .query_map(params![since, TOP], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut out = Vec::new();
            for (id, count) in rows {
                if let Some(track) = query_track(c, &covers, id)? {
                    out.push(TopTrack { track, count });
                }
            }
            Ok(out)
        };
        let items = |sql: &str| -> rusqlite::Result<Vec<TopItem>> {
            let mut st = c.prepare_cached(sql)?;
            let rows = st.query_map(params![since, TOP], |r| {
                Ok(TopItem {
                    name: r.get(0)?,
                    count: r.get(1)?,
                })
            })?;
            rows.collect()
        };
        let top_tracks = tracks(
            "SELECT track_id, sum(completed) n FROM play_history WHERE started_at >= ?1
             GROUP BY track_id HAVING n > 0 ORDER BY n DESC, max(started_at) DESC LIMIT ?2",
        )?;
        let most_skipped = tracks(
            "SELECT track_id, sum(skipped) n FROM play_history WHERE started_at >= ?1
             GROUP BY track_id HAVING n > 0 ORDER BY n DESC LIMIT ?2",
        )?;
        let top_artists = items(
            "SELECT a.name, sum(h.completed) n FROM play_history h
             JOIN track_artist ta ON ta.track_id = h.track_id AND ta.role = 'main'
             JOIN artist a ON a.id = ta.artist_id
             WHERE h.started_at >= ?1 GROUP BY a.id HAVING n > 0 ORDER BY n DESC LIMIT ?2",
        )?;
        let top_genres = items(
            "SELECT g.name, sum(h.completed) n FROM play_history h
             JOIN track_genre tg ON tg.track_id = h.track_id
             JOIN genre g ON g.id = tg.genre_id
             WHERE h.started_at >= ?1 GROUP BY g.id HAVING n > 0 ORDER BY n DESC LIMIT ?2",
        )?;
        let mut recent = Vec::new();
        {
            let mut st = c.prepare_cached(
                "SELECT track_id, started_at + played_ms, skipped FROM play_history
                 ORDER BY started_at DESC LIMIT 20",
            )?;
            let rows: Vec<(i64, i64, bool)> = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (id, at_ms, skipped) in rows {
                if let Some(track) = query_track(c, &covers, id)? {
                    recent.push(RecentPlay {
                        track,
                        at_ms,
                        skipped,
                    });
                }
            }
        }
        let span = if days == 0 { 60 } else { days.min(60) } as i64;
        let first_day = now / DAY_MS - span + 1;
        let mut per_day = vec![0u32; span as usize];
        {
            let mut st = c.prepare_cached(
                "SELECT started_at / ?2, sum(completed) FROM play_history
                 WHERE started_at >= ?1 GROUP BY 1",
            )?;
            let rows = st.query_map(params![first_day * DAY_MS, DAY_MS], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, u32>(1)?))
            })?;
            for row in rows {
                let (day, n) = row?;
                if let Some(slot) = per_day.get_mut((day - first_day) as usize) {
                    *slot = n;
                }
            }
        }
        Ok(ListenStats {
            plays,
            skips,
            listened_ms,
            distinct_tracks,
            top_tracks,
            top_artists,
            top_genres,
            most_skipped,
            recent,
            per_day,
        })
    })
}

/// Rates a track with 0–5 stars (0 = not rated).
pub fn set_rating(track_id: i64, stars: u8) -> Result<()> {
    let value = (stars.min(5) > 0).then_some(i64::from(stars.min(5)) * 2);
    write(move |c| {
        c.execute(
            "UPDATE track SET rating = ?2 WHERE id = ?1",
            params![track_id, value],
        )
    })??;
    Ok(())
}
