//! Note operations shared by the piano roll and tests: sorting with a selection, quantize,
//! humanize.

use gt_core::Note;

/// Sorts notes by (start, key), carrying a parallel selection flag with each note.
pub fn sort_with_selection(notes: &mut Vec<Note>, selected: &mut Vec<bool>) {
    selected.resize(notes.len(), false);
    let mut pairs: Vec<(Note, bool)> = notes.drain(..).zip(selected.drain(..)).collect();
    pairs.sort_by_key(|(n, _)| (n.start, n.key));
    for (n, s) in pairs {
        notes.push(n);
        selected.push(s);
    }
}

/// Moves note starts towards the nearest multiple of `grid` ticks. `strength` 1.0 snaps fully,
/// 0.5 halves the distance. Lengths are kept. Only notes with `selected[i]` (or all notes if
/// nothing is selected) are touched.
pub fn quantize(notes: &mut [Note], selected: &[bool], grid: i64, strength: f32) {
    if grid <= 0 {
        return;
    }
    let any = selected.iter().any(|&s| s);
    let s = f64::from(strength.clamp(0.0, 1.0));
    for (i, n) in notes.iter_mut().enumerate() {
        if any && !selected.get(i).copied().unwrap_or(false) {
            continue;
        }
        let target = (n.start as f64 / grid as f64).round() as i64 * grid;
        n.start = n.start + ((target - n.start) as f64 * s).round() as i64;
    }
}

/// Randomly offsets starts by up to ±`timing` ticks and scales velocities by up to
/// ±`velocity` (fraction), deterministically from `seed`. Starts never go below 0.
pub fn humanize(notes: &mut [Note], selected: &[bool], timing: i64, velocity: f32, seed: u64) {
    let any = selected.iter().any(|&s| s);
    let mut rng = seed | 1;
    let mut next = move || {
        // xorshift64*; uniform in -1..1
        rng ^= rng >> 12;
        rng ^= rng << 25;
        rng ^= rng >> 27;
        let x = rng.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (x >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    };
    for (i, n) in notes.iter_mut().enumerate() {
        if any && !selected.get(i).copied().unwrap_or(false) {
            continue;
        }
        n.start = (n.start + (next() * timing as f64).round() as i64).max(0);
        let v = n.velocity * (1.0 + (next() as f32) * velocity);
        n.velocity = v.clamp(0.01, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(start: i64, key: u8) -> Note {
        Note {
            start,
            length: 100,
            key,
            velocity: 0.5,
        }
    }

    #[test]
    fn sort_carries_selection() {
        let mut n = vec![note(300, 60), note(100, 62), note(100, 61)];
        let mut s = vec![true, false, false];
        sort_with_selection(&mut n, &mut s);
        assert_eq!(
            n.iter().map(|n| n.start).collect::<Vec<_>>(),
            [100, 100, 300]
        );
        assert_eq!(n[0].key, 61);
        assert_eq!(s, [false, false, true]);
    }

    #[test]
    fn quantize_full_and_half_strength() {
        let mut n = vec![note(250, 60), note(470, 60)];
        quantize(&mut n, &[], 240, 1.0);
        assert_eq!((n[0].start, n[1].start), (240, 480));
        let mut n = vec![note(260, 60)];
        quantize(&mut n, &[], 240, 0.5);
        assert_eq!(n[0].start, 250);
        // Triplet grid (1/8T = 320 ticks), only the selected note.
        let mut n = vec![note(300, 60), note(300, 60)];
        quantize(&mut n, &[false, true], 320, 1.0);
        assert_eq!((n[0].start, n[1].start), (300, 320));
    }

    #[test]
    fn humanize_stays_in_bounds_and_is_deterministic() {
        let base: Vec<_> = (0..200).map(|i| note(i * 240, 60)).collect();
        let mut a = base.clone();
        humanize(&mut a, &[], 20, 0.2, 7);
        let mut b = base.clone();
        humanize(&mut b, &[], 20, 0.2, 7);
        assert_eq!(a, b);
        assert_ne!(a, base);
        for (x, y) in a.iter().zip(&base) {
            assert!((x.start - y.start).abs() <= 20 && x.start >= 0);
            assert!(x.velocity >= 0.4 - 1e-6 && x.velocity <= 0.6 + 1e-6);
        }
    }
}
