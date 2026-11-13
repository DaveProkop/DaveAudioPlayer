//! Smart playlists: rules stored as JSON in `playlist.rules`, compiled to SQL here.
//!
//! Only known fields and operators are accepted, and every value goes in as a SQL
//! parameter, so a stored rule can never inject SQL.

use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};
use serde::{Deserialize, Serialize};

use crate::catalog::now_ms;
use crate::db::{DbError, Result};

const DAY_MS: i64 = 24 * 3600 * 1000;

/// A smart playlist definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SmartRules {
    /// All rules must hold (`true`) or any of them.
    #[serde(default = "yes")]
    pub match_all: bool,
    pub rules: Vec<Rule>,
    /// How tracks are ordered.
    #[serde(default)]
    pub order: Order,
    /// At most this many tracks (0 = all).
    #[serde(default)]
    pub limit: u32,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub field: Field,
    pub op: Op,
    /// Number or text, depending on the field.
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    Title,
    Artist,
    Album,
    Genre,
    Year,
    /// Stars, 0–5.
    Rating,
    PlayCount,
    SkipCount,
    /// Days since the track was last played (never played = infinitely long ago).
    LastPlayedDays,
    /// Days since the track was added to the library.
    AddedDays,
    Bpm,
    /// Camelot key, e.g. 8A.
    Key,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Is,
    IsNot,
    Contains,
    AtLeast,
    AtMost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Order {
    #[default]
    Library,
    Random,
    MostPlayed,
    RecentlyPlayed,
    RecentlyAdded,
    Rating,
}

impl SmartRules {
    pub fn from_json(json: &str) -> Result<Self> {
        serde_json::from_str(json).map_err(|e| DbError::File(format!("smart playlist rules: {e}")))
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("rules serialize")
    }
}

/// `WHERE` condition and parameters for one rule; `None` for a combination that makes no
/// sense (e.g. "title at least").
fn condition(rule: &Rule, now: i64) -> Option<(String, Vec<Value>)> {
    let text = || Value::Text(rule.value.trim().to_string());
    let like = || Value::Text(format!("%{}%", rule.value.trim()));
    let number = || rule.value.trim().replace(',', ".").parse::<f64>().ok();
    let cmp = |expr: &str, scale: f64| -> Option<(String, Vec<Value>)> {
        let v = number()? * scale;
        let op = match rule.op {
            Op::Is => "=",
            Op::IsNot => "!=",
            Op::AtLeast => ">=",
            Op::AtMost => "<=",
            Op::Contains => return None,
        };
        Some((format!("{expr} {op} ?"), vec![Value::Real(v)]))
    };
    let names = |exists: &str| -> Option<(String, Vec<Value>)> {
        let (cond, value) = match rule.op {
            Op::Is => ("name = ? COLLATE NOCASE", text()),
            Op::IsNot => return Some((format!("NOT EXISTS ({exists} AND name = ? COLLATE NOCASE)"), vec![text()])),
            Op::Contains => ("name LIKE ?", like()),
            _ => return None,
        };
        Some((format!("EXISTS ({exists} AND {cond})"), vec![value]))
    };
    let column_text = |expr: &str| -> Option<(String, Vec<Value>)> {
        match rule.op {
            Op::Is => Some((format!("{expr} = ? COLLATE NOCASE"), vec![text()])),
            Op::IsNot => Some((format!("{expr} != ? COLLATE NOCASE"), vec![text()])),
            Op::Contains => Some((format!("{expr} LIKE ?"), vec![like()])),
            _ => None,
        }
    };
    match rule.field {
        Field::Title => column_text("t.title"),
        Field::Album => column_text("coalesce(al.title, '')"),
        Field::Key => column_text("coalesce(an.key_camelot, '')"),
        Field::Artist => names(
            "SELECT 1 FROM track_artist ta JOIN artist a ON a.id = ta.artist_id
             WHERE ta.track_id = t.id",
        )
        .map(|(c, v)| (c.replace("name", "a.name"), v)),
        Field::Genre => names(
            "SELECT 1 FROM track_genre tg JOIN genre g ON g.id = tg.genre_id
             WHERE tg.track_id = t.id",
        )
        .map(|(c, v)| (c.replace("name", "g.name"), v)),
        Field::Year => cmp("t.year", 1.0),
        Field::Rating => cmp("coalesce(t.rating, 0)", 2.0),
        Field::PlayCount => cmp("t.play_count", 1.0),
        Field::SkipCount => cmp("t.skip_count", 1.0),
        Field::Bpm => cmp("an.bpm", 1.0),
        Field::LastPlayedDays => {
            // "days since" on a never played track: as if infinitely long ago.
            let expr = format!("(({now} - coalesce(t.last_played, 0)) / {DAY_MS}.0)");
            cmp(&expr, 1.0)
        }
        Field::AddedDays => cmp(&format!("(({now} - t.added_at) / {DAY_MS}.0)"), 1.0),
    }
}

/// The tracks a smart playlist holds, in its order.
pub fn evaluate(conn: &Connection, rules: &SmartRules) -> Result<Vec<i64>> {
    let now = now_ms();
    let mut conds = Vec::new();
    let mut values = Vec::new();
    for rule in &rules.rules {
        if let Some((c, v)) = condition(rule, now) {
            conds.push(format!("({c})"));
            values.extend(v);
        }
    }
    let filter = if conds.is_empty() {
        "1".to_string()
    } else {
        conds.join(if rules.match_all { " AND " } else { " OR " })
    };
    let order = match rules.order {
        Order::Library => "coalesce(aa.name_sort, ''), coalesce(al.title_sort, ''), t.disc_no, t.track_no, t.title_sort",
        Order::Random => "random()",
        Order::MostPlayed => "t.play_count DESC, t.title_sort",
        Order::RecentlyPlayed => "coalesce(t.last_played, 0) DESC",
        Order::RecentlyAdded => "t.added_at DESC",
        Order::Rating => "coalesce(t.rating, 0) DESC, t.play_count DESC",
    };
    let limit = if rules.limit > 0 { format!("LIMIT {}", rules.limit) } else { String::new() };
    let sql = format!(
        "SELECT t.id FROM track t
         JOIN file f ON f.id = t.file_id AND f.missing = 0
         LEFT JOIN album al ON al.id = t.album_id
         LEFT JOIN artist aa ON aa.id = al.album_artist_id
         LEFT JOIN analysis an ON an.track_id = t.id
         WHERE {filter} ORDER BY {order} {limit}"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(values), |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    fn rule(field: Field, op: Op, value: &str) -> Rule {
        Rule {
            field,
            op,
            value: value.into(),
        }
    }

    /// Tracks: (title, genre, rating 0-10, play_count, last played days ago).
    fn library(conn: &Connection) -> Vec<i64> {
        conn.execute("INSERT INTO library_root (path, added_at) VALUES ('/m', 0)", [])
            .unwrap();
        conn.execute("INSERT INTO genre (name) VALUES ('Rock'), ('Jazz')", []).unwrap();
        let now = now_ms();
        let rows = [
            ("Alpha", 1, 8, 10, Some(1)),
            ("Beta", 2, 10, 2, Some(40)),
            ("Gamma", 1, 4, 0, None),
            ("Delta", 2, 0, 5, Some(3)),
        ];
        rows.iter()
            .enumerate()
            .map(|(i, (title, genre, rating, plays, days))| {
                conn.execute(
                    "INSERT INTO file (root_id, rel_path, size, mtime, format, scanned_at)
                     VALUES (1, ?1, 0, 0, 'mp3', 0)",
                    [format!("{i}.mp3")],
                )
                .unwrap();
                let file = conn.last_insert_rowid();
                conn.execute(
                    "INSERT INTO track (file_id, title, title_sort, rating, play_count, last_played, added_at)
                     VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![
                        file,
                        title,
                        rating,
                        plays,
                        days.map(|d: i64| now - d * DAY_MS),
                        now - i as i64 * DAY_MS
                    ],
                )
                .unwrap();
                let id = conn.last_insert_rowid();
                conn.execute(
                    "INSERT INTO track_genre (track_id, genre_id) VALUES (?1, ?2)",
                    [id, *genre],
                )
                .unwrap();
                id
            })
            .collect()
    }

    #[test]
    fn rules_select_and_order() {
        let conn = open_in_memory().unwrap();
        let t = library(&conn);
        // "Rated 4+ stars and not played for 30 days": Beta (5 stars, 40 days).
        let rules = SmartRules {
            match_all: true,
            rules: vec![
                rule(Field::Rating, Op::AtLeast, "4"),
                rule(Field::LastPlayedDays, Op::AtLeast, "30"),
            ],
            order: Order::Library,
            limit: 0,
        };
        assert_eq!(evaluate(&conn, &rules).unwrap(), [t[1]]);

        // Rock or never played, most played first.
        let rules = SmartRules {
            match_all: false,
            rules: vec![
                rule(Field::Genre, Op::Is, "rock"),
                rule(Field::PlayCount, Op::Is, "0"),
            ],
            order: Order::MostPlayed,
            limit: 0,
        };
        assert_eq!(evaluate(&conn, &rules).unwrap(), [t[0], t[2]]);

        // Contains, a limit, and a round trip through JSON.
        let rules = SmartRules {
            match_all: true,
            rules: vec![rule(Field::Title, Op::Contains, "ta")],
            order: Order::RecentlyAdded,
            limit: 2,
        };
        let back = SmartRules::from_json(&rules.to_json()).unwrap();
        assert_eq!(back, rules);
        // Beta was added a day ago, Delta three days ago.
        assert_eq!(evaluate(&conn, &back).unwrap(), [t[1], t[3]]);
    }

    #[test]
    fn nonsense_rules_are_ignored_and_injection_is_inert() {
        let conn = open_in_memory().unwrap();
        let t = library(&conn);
        let rules = SmartRules {
            match_all: true,
            rules: vec![
                rule(Field::Title, Op::AtLeast, "5"),
                rule(Field::Genre, Op::Is, "x'); DROP TABLE track; --"),
            ],
            order: Order::Library,
            limit: 0,
        };
        assert!(evaluate(&conn, &rules).unwrap().is_empty());
        let still: i64 = conn.query_row("SELECT count(*) FROM track", [], |r| r.get(0)).unwrap();
        assert_eq!(still, t.len() as i64);
        let parsed = SmartRules::from_json(r#"{"rules": []}"#).unwrap();
        assert!(parsed.match_all && parsed.limit == 0);
    }
}
