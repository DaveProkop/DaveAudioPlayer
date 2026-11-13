//! LRC lyrics: `[mm:ss.xx]text` lines, several stamps per line, and an `[offset:±ms]` tag.

/// One timed line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LrcLine {
    pub ms: u64,
    pub text: String,
}

/// Parses LRC text into lines sorted by time; lines without a time stamp are dropped.
/// Returns an empty list for text that is not LRC.
pub fn parse_lrc(body: &str) -> Vec<LrcLine> {
    let mut offset_ms: i64 = 0;
    let mut lines = Vec::new();
    for raw in body.lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        while let Some(tag) = rest.strip_prefix('[') {
            let Some(end) = tag.find(']') else { break };
            let (inner, after) = (&tag[..end], &tag[end + 1..]);
            if let Some(v) = inner.strip_prefix("offset:") {
                offset_ms = v.trim().parse().unwrap_or(0);
            } else if let Some(ms) = parse_stamp(inner) {
                stamps.push(ms);
            }
            rest = after;
        }
        let text = rest.trim().to_string();
        for ms in stamps {
            // A positive offset shows the lyrics earlier.
            let at = (ms as i64 - offset_ms).max(0) as u64;
            lines.push(LrcLine {
                ms: at,
                text: text.clone(),
            });
        }
    }
    lines.sort_by_key(|l| l.ms);
    lines
}

/// `mm:ss`, `mm:ss.xx` or `mm:ss.xxx` in milliseconds.
fn parse_stamp(s: &str) -> Option<u64> {
    let (min, sec) = s.split_once(':')?;
    let min: u64 = min.trim().parse().ok()?;
    let (whole, frac) = sec.split_once(['.', ':']).unwrap_or((sec, ""));
    let whole: u64 = whole.trim().parse().ok()?;
    if whole >= 60 {
        return None;
    }
    let frac_ms = match frac.len() {
        0 => 0,
        1 => frac.parse::<u64>().ok()? * 100,
        2 => frac.parse::<u64>().ok()? * 10,
        _ => frac[..3].parse::<u64>().ok()?,
    };
    Some(min * 60_000 + whole * 1000 + frac_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stamps_repeats_and_offsets() {
        let lines = parse_lrc(
            "[ar:Someone]\n[offset:500]\n[00:12.30]Hello\n[00:05.00][00:20.5]Chorus\nno stamp\n[01:02.345] Last ",
        );
        let got: Vec<(u64, &str)> = lines.iter().map(|l| (l.ms, l.text.as_str())).collect();
        assert_eq!(
            got,
            [(4500, "Chorus"), (11_800, "Hello"), (20_000, "Chorus"), (61_845, "Last")]
        );
        assert!(parse_lrc("just plain lyrics\nsecond line").is_empty());
    }
}
