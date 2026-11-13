//! Smart Mix ordering: puts tracks in an order where each one mixes well into the next,
//! by key (Camelot wheel), tempo and energy.

use crate::key::Camelot;

/// What the ordering knows about a track.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MixTrack {
    pub key: Option<Camelot>,
    pub bpm: Option<f64>,
    /// Energy 0–1 ([`crate::track_energy`]).
    pub energy: Option<f64>,
}

/// Cost of an unknown key or tempo: about as bad as a mediocre match.
const UNKNOWN: f64 = 1.0;

/// Key part of a transition's cost: 0 for the same key, small for the relative major/minor
/// or a step around the wheel, growing with the distance on the wheel.
pub fn key_cost(a: Option<Camelot>, b: Option<Camelot>) -> f64 {
    let (Some(a), Some(b)) = (a, b) else {
        return UNKNOWN;
    };
    let step = (i32::from(a.number) - i32::from(b.number)).rem_euclid(12);
    let distance = f64::from(step.min(12 - step)); // 0–6 around the wheel
    match (distance as i32, a.minor == b.minor) {
        (0, true) => 0.0,
        (0, false) => 0.3,
        (1, true) => 0.5,
        (1, false) => 1.5,
        (_, same_mode) => 1.0 + distance * 0.7 + if same_mode { 0.0 } else { 0.5 },
    }
}

/// Tempo part of a transition's cost: the gap in percent (half / double time counts as
/// close), 1 unit per 3 %, capped.
pub fn tempo_cost(a: Option<f64>, b: Option<f64>) -> f64 {
    let (Some(a), Some(b)) = (a, b) else {
        return UNKNOWN;
    };
    if a <= 0.0 || b <= 0.0 {
        return UNKNOWN;
    }
    let gap = [1.0, 2.0, 0.5]
        .iter()
        .map(|r| (a / (b * r) - 1.0).abs())
        .fold(f64::INFINITY, f64::min);
    (gap / 0.03).min(4.0)
}

/// Which way the energy should go along the mix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EnergyTrend {
    /// Just no big jumps.
    #[default]
    Steady,
    /// Build up (party): drops cost double.
    Rising,
    /// Wind down (chill): rises cost double.
    Falling,
}

/// Energy part of a transition's cost: 1 unit per 0.15 jump (double against the trend),
/// capped; nothing when unknown (the energy is a soft preference, key and tempo matter
/// more).
pub fn energy_cost(a: Option<f64>, b: Option<f64>, trend: EnergyTrend) -> f64 {
    let (Some(a), Some(b)) = (a, b) else {
        return 0.0;
    };
    let against = match trend {
        EnergyTrend::Steady => false,
        EnergyTrend::Rising => b < a,
        EnergyTrend::Falling => b > a,
    };
    let cost = (a - b).abs() / 0.15 * if against { 2.0 } else { 1.0 };
    cost.min(if against { 4.0 } else { 3.0 })
}

pub fn transition_cost(a: &MixTrack, b: &MixTrack, trend: EnergyTrend) -> f64 {
    key_cost(a.key, b.key) + tempo_cost(a.bpm, b.bpm) + energy_cost(a.energy, b.energy, trend)
}

fn path_cost(
    start: Option<&MixTrack>,
    tracks: &[MixTrack],
    order: &[usize],
    trend: EnergyTrend,
) -> f64 {
    let mut cost = 0.0;
    let mut prev = start;
    for &i in order {
        if let Some(p) = prev {
            cost += transition_cost(p, &tracks[i], trend);
        }
        prev = Some(&tracks[i]);
    }
    cost
}

/// With an energy trend, 2-opt reverses stretches of at most this many tracks.
const MAX_DIRECTED_STRETCH: usize = 32;

/// An order of `tracks` (indices) that keeps transitions cheap, starting after `start`
/// (the playing track) if given, with the energy going the `trend` way. Greedy nearest
/// neighbour, then 2-opt (reversing a stretch also reverses its energy direction, which
/// the costs account for) and moving single tracks.
pub fn mix_order(start: Option<&MixTrack>, tracks: &[MixTrack], trend: EnergyTrend) -> Vec<usize> {
    let n = tracks.len();
    if n < 2 {
        return (0..n).collect();
    }
    // Greedy: always the cheapest next track (ties keep the original order).
    let mut used = vec![false; n];
    let mut order = Vec::with_capacity(n);
    let mut prev = start.copied();
    for _ in 0..n {
        let next = (0..n)
            .filter(|&i| !used[i])
            .min_by(|&i, &j| {
                let ci = prev.map_or(0.0, |p| transition_cost(&p, &tracks[i], trend));
                let cj = prev.map_or(0.0, |p| transition_cost(&p, &tracks[j], trend));
                ci.total_cmp(&cj).then(i.cmp(&j))
            })
            .expect("a track left");
        used[next] = true;
        order.push(next);
        prev = Some(tracks[next]);
    }
    // 2-opt: reverse a stretch when that makes the whole path cheaper (bounded passes).
    // With an energy trend the costs are not symmetric: the reversed stretch's own edges
    // change too, so they are summed, and stretches are kept short to stay fast.
    let directed = trend != EnergyTrend::Steady;
    let max_stretch = if directed { MAX_DIRECTED_STRETCH } else { n };
    let edge = |a: usize, b: usize| transition_cost(&tracks[a], &tracks[b], trend);
    let cost_between = |a: Option<usize>, b: usize, order: &[usize]| -> f64 {
        match a {
            Some(a) => transition_cost(&tracks[order[a]], &tracks[order[b]], trend),
            None => start.map_or(0.0, |s| transition_cost(s, &tracks[order[b]], trend)),
        }
    };
    for _ in 0..20 {
        let mut improved = false;
        for i in 0..n - 1 {
            for j in i + 1..n.min(i + 1 + max_stretch) {
                // Reversing order[i..=j] changes the edges (i-1, i) and (j, j+1).
                let before_i = i.checked_sub(1);
                let mut old = cost_between(before_i, i, &order)
                    + if j + 1 < n {
                        cost_between(Some(j), j + 1, &order)
                    } else {
                        0.0
                    };
                let mut new = cost_between(before_i, j, &order)
                    + if j + 1 < n {
                        edge(order[i], order[j + 1])
                    } else {
                        0.0
                    };
                if directed {
                    for k in i..j {
                        old += edge(order[k], order[k + 1]);
                        new += edge(order[k + 1], order[k]);
                    }
                }
                if new + 1e-9 < old {
                    order[i..=j].reverse();
                    improved = true;
                }
            }
        }
        if !improved {
            break;
        }
    }
    // Relocate: move single tracks where they fit best. Fixes what reversals cannot, such
    // as a calm track left at the end of a mix that should build up.
    let link = |a: Option<usize>, b: Option<usize>| match (a, b) {
        (_, None) => 0.0,
        (Some(a), Some(b)) => edge(a, b),
        (None, Some(b)) => start.map_or(0.0, |s| transition_cost(s, &tracks[b], trend)),
    };
    for _ in 0..20 {
        let mut improved = false;
        for k in 0..n {
            let x = order[k];
            let before = k.checked_sub(1).map(|i| order[i]);
            let after = order.get(k + 1).copied();
            let saved = link(before, Some(x)) + link(Some(x), after) - link(before, after);
            let mut rest = order.clone();
            rest.remove(k);
            let mut best: Option<(f64, usize)> = None;
            for p in (0..=rest.len()).filter(|&p| p != k) {
                let a = p.checked_sub(1).map(|i| rest[i]);
                let b = rest.get(p).copied();
                let added = link(a, Some(x)) + link(Some(x), b) - link(a, b);
                if added + 1e-9 < saved && best.is_none_or(|(c, _)| added < c) {
                    best = Some((added, p));
                }
            }
            if let Some((_, p)) = best {
                rest.insert(p, x);
                order = rest;
                improved = true;
            }
        }
        if !improved {
            break;
        }
    }
    debug_assert!(path_cost(start, tracks, &order, trend).is_finite());
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(key: &str, bpm: f64) -> MixTrack {
        MixTrack {
            key: Camelot::parse(key),
            bpm: Some(bpm),
            energy: None,
        }
    }

    #[test]
    fn the_trend_orders_by_energy() {
        let e = |energy| MixTrack {
            energy: Some(energy),
            ..t("8A", 128.0)
        };
        let tracks = [e(0.5), e(0.9), e(0.2), e(0.7)];
        let energies = |order: Vec<usize>| {
            order
                .iter()
                .map(|&i| tracks[i].energy.unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            energies(mix_order(None, &tracks, EnergyTrend::Rising)),
            [0.2, 0.5, 0.7, 0.9]
        );
        assert_eq!(
            energies(mix_order(None, &tracks, EnergyTrend::Falling)),
            [0.9, 0.7, 0.5, 0.2]
        );
        assert!(
            energy_cost(Some(0.8), Some(0.5), EnergyTrend::Rising)
                > energy_cost(Some(0.5), Some(0.8), EnergyTrend::Rising)
        );
    }

    #[test]
    fn energy_jumps_cost_but_unknown_energy_does_not() {
        assert_eq!(energy_cost(Some(0.8), Some(0.8), EnergyTrend::Steady), 0.0);
        assert!((energy_cost(Some(0.8), Some(0.5), EnergyTrend::Steady) - 2.0).abs() < 1e-9);
        assert_eq!(energy_cost(Some(0.9), Some(0.1), EnergyTrend::Steady), 3.0);
        assert_eq!(energy_cost(None, Some(0.5), EnergyTrend::Steady), 0.0);
        // Same key and tempo: the closer energy goes next.
        let start = MixTrack {
            energy: Some(0.9),
            ..t("8A", 128.0)
        };
        let calm = MixTrack {
            energy: Some(0.3),
            ..t("8A", 128.0)
        };
        let hot = MixTrack {
            energy: Some(0.85),
            ..t("8A", 128.0)
        };
        assert_eq!(
            mix_order(Some(&start), &[calm, hot], EnergyTrend::Steady),
            [1, 0]
        );
    }

    #[test]
    fn costs() {
        let k = |s| Camelot::parse(s);
        assert_eq!(key_cost(k("8A"), k("8A")), 0.0);
        assert!(key_cost(k("8A"), k("9A")) < key_cost(k("8A"), k("10A")));
        assert!(key_cost(k("8A"), k("8B")) < key_cost(k("8A"), k("9B")));
        assert_eq!(key_cost(k("12A"), k("1A")), 0.5, "the wheel wraps");
        assert_eq!(tempo_cost(Some(128.0), Some(128.0)), 0.0);
        assert!(
            tempo_cost(Some(87.0), Some(174.0)) < 0.01,
            "half time is close"
        );
        assert_eq!(tempo_cost(Some(90.0), Some(140.0)), 4.0, "capped");
    }

    #[test]
    fn walks_the_wheel_instead_of_jumping() {
        // Shuffled: 8A, 11A, 9A, 10A, all 124 BPM; start at 7A.
        let tracks = [
            t("8A", 124.0),
            t("11A", 124.0),
            t("9A", 124.0),
            t("10A", 124.0),
        ];
        let order = mix_order(Some(&t("7A", 124.0)), &tracks, EnergyTrend::Steady);
        assert_eq!(order, [0, 2, 3, 1]);
    }

    #[test]
    fn groups_similar_tempos() {
        let tracks = [
            t("8A", 174.0),
            t("8A", 124.0),
            t("8A", 172.0),
            t("8A", 126.0),
        ];
        let order = mix_order(Some(&t("8A", 125.0)), &tracks, EnergyTrend::Steady);
        let bpms: Vec<f64> = order.iter().map(|&i| tracks[i].bpm.unwrap()).collect();
        assert!(bpms[0] < 130.0 && bpms[1] < 130.0, "{bpms:?}");
        assert!(bpms[2] > 170.0 && bpms[3] > 170.0);
    }

    #[test]
    fn is_a_permutation_and_never_worse_than_given() {
        let keys = ["1A", "5B", "8A", "3A", "12B", "7A", "8B", "2A", "9A", "4B"];
        let tracks: Vec<MixTrack> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| t(k, 110.0 + (i * 7 % 40) as f64))
            .collect();
        let order = mix_order(None, &tracks, EnergyTrend::Steady);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..tracks.len()).collect::<Vec<_>>());
        let identity: Vec<usize> = (0..tracks.len()).collect();
        assert!(
            path_cost(None, &tracks, &order, EnergyTrend::Steady)
                <= path_cost(None, &tracks, &identity, EnergyTrend::Steady)
        );
    }
}
