//! Musical keys in Camelot notation (the DJ wheel for harmonic mixing), parsing of the key
//! notations found in tags, and key detection from a chromagram.

use std::fmt;

/// A key: tonic pitch class (C = 0 … B = 11) and mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub tonic: u8,
    pub minor: bool,
}

/// A Camelot wheel position: 1–12 and A (minor) / B (major).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Camelot {
    pub number: u8,
    pub minor: bool,
}

const NOTES: [&str; 12] = [
    "C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
];

impl Key {
    pub fn camelot(self) -> Camelot {
        // The relative major shares the wheel number; majors step by fifths from C = 8B.
        let major_tonic = if self.minor {
            (self.tonic + 3) % 12
        } else {
            self.tonic
        };
        let number = (u32::from(major_tonic) * 7 % 12 + 7) % 12 + 1;
        Camelot {
            number: number as u8,
            minor: self.minor,
        }
    }

    /// "Am", "F#", "Bbm".
    pub fn name(self) -> String {
        format!(
            "{}{}",
            NOTES[self.tonic as usize],
            if self.minor { "m" } else { "" }
        )
    }
}

impl Camelot {
    pub fn key(self) -> Key {
        // Inverse of `Key::camelot`: number n → major tonic (n − 8) · 7 (mod 12).
        let major_tonic = ((i32::from(self.number) - 8).rem_euclid(12) * 7 % 12) as u8;
        Key {
            tonic: if self.minor {
                (major_tonic + 9) % 12
            } else {
                major_tonic
            },
            minor: self.minor,
        }
    }

    /// Mixes well with `other`: the same key, a step around the wheel, or the relative
    /// major/minor.
    pub fn compatible(self, other: Camelot) -> bool {
        let step = (i32::from(self.number) - i32::from(other.number)).rem_euclid(12);
        if self.minor == other.minor {
            step == 0 || step == 1 || step == 11
        } else {
            step == 0
        }
    }

    /// Parses "8A", "08b".
    pub fn parse(s: &str) -> Option<Camelot> {
        let s = s.trim();
        let (num, letter) = s.split_at(s.len().checked_sub(1)?);
        let number: u8 = num.trim().parse().ok()?;
        let minor = match letter {
            "A" | "a" => true,
            "B" | "b" => false,
            _ => return None,
        };
        (1..=12)
            .contains(&number)
            .then_some(Camelot { number, minor })
    }
}

impl fmt::Display for Camelot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.number, if self.minor { 'A' } else { 'B' })
    }
}

/// Reads a key as written by DJ and tagging software: Camelot ("8A"), Open Key ("1m",
/// "1d"), or a note name ("Am", "A minor", "F#m", "Bbmin", "Cmaj", "Ebm"). `None` for
/// anything else ("o" = off-key, empty, garbage).
pub fn parse_key_tag(tag: &str) -> Option<Camelot> {
    let s = tag.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(c) = Camelot::parse(s) {
        return Some(c);
    }
    // Open Key (Traktor): 1d = C major = 8B, 1m = A minor = 8A.
    if let Some(last) = s.chars().last()
        && matches!(last, 'd' | 'm' | 'D' | 'M')
        && let Ok(n) = s[..s.len() - 1].trim().parse::<u8>()
        && (1..=12).contains(&n)
    {
        return Some(Camelot {
            number: (n - 1 + 7) % 12 + 1,
            minor: matches!(last, 'm' | 'M'),
        });
    }
    // Note name: letter, accidentals, then the mode.
    let mut chars = s.chars().peekable();
    let letter = chars.next()?.to_ascii_uppercase();
    let mut tonic: i32 = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    while let Some(&c) = chars.peek() {
        match c {
            '#' | '♯' => tonic += 1,
            'b' | '♭' => tonic -= 1,
            _ => break,
        }
        chars.next();
    }
    let mode: String = chars.collect::<String>().trim().to_lowercase();
    let minor = match mode.as_str() {
        "" | "maj" | "major" | "dur" => false,
        "m" | "min" | "minor" | "moll" => true,
        _ => return None,
    };
    Some(
        Key {
            tonic: tonic.rem_euclid(12) as u8,
            minor,
        }
        .camelot(),
    )
}

// ----- detection -----------------------------------------------------------------------

/// Major and minor key profiles: expected weight of each pitch class, index 0 = tonic.
#[derive(Debug, Clone, Copy)]
pub struct KeyProfiles {
    pub name: &'static str,
    pub major: [f64; 12],
    pub minor: [f64; 12],
}

// 3.14 is a profile weight, not an approximation of pi.
#[allow(clippy::approx_constant)]
pub const TEMPERLEY: KeyProfiles = KeyProfiles {
    name: "temperley",
    major: [
        7.24, 3.50, 3.58, 2.85, 5.82, 4.56, 2.45, 6.99, 3.34, 4.56, 4.07, 4.46,
    ],
    minor: [
        7.00, 3.14, 4.36, 5.40, 3.67, 4.09, 3.91, 6.20, 3.63, 2.87, 5.35, 3.83,
    ],
};

pub const KRUMHANSL: KeyProfiles = KeyProfiles {
    name: "krumhansl",
    major: [
        6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
    ],
    minor: [
        6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
    ],
};

/// KeyFinder (Ibrahim Sha'ath).
pub const SHAATH: KeyProfiles = KeyProfiles {
    name: "shaath",
    major: [6.6, 2.0, 3.5, 2.3, 4.6, 4.0, 2.5, 5.2, 2.4, 3.7, 2.3, 3.4],
    minor: [6.5, 2.7, 3.5, 5.4, 2.6, 3.5, 2.5, 5.2, 4.0, 2.7, 4.3, 3.2],
};

/// Electronic dance music (Faraldo et al., "edma").
pub const EDMA: KeyProfiles = KeyProfiles {
    name: "edma",
    major: [
        0.1652, 0.0475, 0.0829, 0.0669, 0.0999, 0.0927, 0.0529, 0.1316, 0.0522, 0.0744, 0.0553,
        0.0784,
    ],
    minor: [
        0.1724, 0.0400, 0.0761, 0.1211, 0.0575, 0.0854, 0.0436, 0.1151, 0.0601, 0.0592, 0.0726,
        0.0632,
    ],
};

pub const ALL_PROFILES: [KeyProfiles; 4] = [TEMPERLEY, KRUMHANSL, SHAATH, EDMA];

/// The profiles used by the app.
pub const DEFAULT_PROFILES: KeyProfiles = TEMPERLEY;

fn correlation(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    let ma = a.iter().sum::<f64>() / 12.0;
    let mb = b.iter().sum::<f64>() / 12.0;
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        let (x, y) = (a[i] - ma, b[i] - mb);
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da == 0.0 || db == 0.0 {
        0.0
    } else {
        num / (da * db).sqrt()
    }
}

/// The best matching key for a chroma vector (energy per pitch class, C = 0), with a
/// confidence 0–1 (how clearly it beats the runner-up). `None` for silence.
pub fn key_from_chroma(chroma: &[f64; 12]) -> Option<(Key, f64)> {
    key_from_chroma_with(chroma, &DEFAULT_PROFILES)
}

/// [`key_from_chroma`] with given profiles.
pub fn key_from_chroma_with(chroma: &[f64; 12], profiles: &KeyProfiles) -> Option<(Key, f64)> {
    if chroma.iter().all(|&c| c <= 0.0) {
        return None;
    }
    let mut scores: Vec<(Key, f64)> = Vec::with_capacity(24);
    for tonic in 0..12u8 {
        for minor in [false, true] {
            let profile = if minor {
                &profiles.minor
            } else {
                &profiles.major
            };
            let mut rotated = [0.0; 12];
            for (i, r) in rotated.iter_mut().enumerate() {
                *r = profile[(i + 12 - tonic as usize) % 12];
            }
            scores.push((Key { tonic, minor }, correlation(chroma, &rotated)));
        }
    }
    scores.sort_by(|a, b| b.1.total_cmp(&a.1));
    let (best, top) = scores[0];
    let second = scores[1].1;
    let confidence = if top <= 0.0 {
        0.0
    } else {
        ((top - second) / top * 10.0).clamp(0.0, 1.0)
    };
    Some((best, confidence))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> String {
        parse_key_tag(s).map(|c| c.to_string()).unwrap_or_default()
    }

    #[test]
    fn camelot_wheel() {
        let c = |tonic, minor| Key { tonic, minor }.camelot().to_string();
        assert_eq!(c(0, false), "8B"); // C
        assert_eq!(c(9, true), "8A"); // Am
        assert_eq!(c(7, false), "9B"); // G
        assert_eq!(c(5, false), "7B"); // F
        assert_eq!(c(11, false), "1B"); // B
        assert_eq!(c(8, true), "1A"); // G#m
        assert_eq!(c(3, true), "2A"); // Ebm
        assert_eq!(c(1, true), "12A"); // C#m
        for tonic in 0..12 {
            for minor in [false, true] {
                let k = Key { tonic, minor };
                assert_eq!(k.camelot().key(), k, "{}", k.name());
            }
        }
    }

    #[test]
    fn tag_notations() {
        assert_eq!(key("8A"), "8A");
        assert_eq!(key("08b"), "8B");
        assert_eq!(key("Am"), "8A");
        assert_eq!(key("A minor"), "8A");
        assert_eq!(key("C"), "8B");
        assert_eq!(key("Cmaj"), "8B");
        assert_eq!(key("F#m"), "11A");
        assert_eq!(key("Gbm"), "11A");
        assert_eq!(key("Bbmin"), "3A");
        assert_eq!(key("Ebm"), "2A");
        assert_eq!(key("1m"), "8A"); // Open Key
        assert_eq!(key("1d"), "8B");
        assert_eq!(key("6m"), "1A");
        for bad in ["", "o", "13A", "Hm", "Xmaj", "8C"] {
            assert_eq!(key(bad), "", "{bad}");
        }
    }

    #[test]
    fn harmonic_compatibility() {
        let c = |s: &str| Camelot::parse(s).unwrap();
        assert!(c("8A").compatible(c("8A")));
        assert!(c("8A").compatible(c("9A")));
        assert!(c("8A").compatible(c("7A")));
        assert!(c("8A").compatible(c("8B")));
        assert!(c("12B").compatible(c("1B")), "the wheel wraps");
        assert!(!c("8A").compatible(c("10A")));
        assert!(!c("8A").compatible(c("9B")));
    }

    #[test]
    fn chroma_of_a_major_and_a_minor_scale() {
        // Weighted like real music: tonic, fifth and third strongest.
        let mut c_major = [0.0; 12];
        for (pc, w) in [
            (0, 9.0),
            (2, 3.0),
            (4, 6.0),
            (5, 3.0),
            (7, 8.0),
            (9, 3.0),
            (11, 2.5),
        ] {
            c_major[pc] = w;
        }
        let (k, conf) = key_from_chroma(&c_major).unwrap();
        assert_eq!(k.camelot().to_string(), "8B");
        assert!(conf > 0.0);

        let mut a_minor = [0.0; 12];
        for (pc, w) in [
            (9, 9.0),
            (11, 3.0),
            (0, 6.0),
            (2, 3.0),
            (4, 8.0),
            (5, 3.0),
            (8, 3.0),
        ] {
            a_minor[pc] = w;
        }
        assert_eq!(
            key_from_chroma(&a_minor).unwrap().0.camelot().to_string(),
            "8A"
        );
        assert!(key_from_chroma(&[0.0; 12]).is_none());
    }
}
