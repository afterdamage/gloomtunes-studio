//! Destructive audio edits for the audio editor (Prompt 32, D146).
//!
//! Every operation works on planar channel data (`Vec<Vec<f32>>`, one vector per channel, all
//! the same length) and a frame range `[start, end)`, which is clamped to the data. They run on
//! the editor's working copy; the editor keeps the previous copies for undo.

use std::ops::Range;

/// Planar audio: one vector per channel.
pub type Planar = Vec<Vec<f32>>;

fn frames(a: &Planar) -> usize {
    a.first().map_or(0, Vec::len)
}

fn clamp(a: &Planar, r: &Range<usize>) -> Range<usize> {
    let n = frames(a);
    let s = r.start.min(n);
    s..r.end.clamp(s, n)
}

/// Keeps only `r`.
pub fn trim(a: &mut Planar, r: Range<usize>) {
    let r = clamp(a, &r);
    for c in a.iter_mut() {
        c.truncate(r.end);
        c.drain(..r.start);
    }
}

/// The audio in `r`.
pub fn copy(a: &Planar, r: Range<usize>) -> Planar {
    let r = clamp(a, &r);
    a.iter().map(|c| c[r.clone()].to_vec()).collect()
}

/// Removes `r` and returns it.
pub fn cut(a: &mut Planar, r: Range<usize>) -> Planar {
    let r = clamp(a, &r);
    a.iter_mut().map(|c| c.drain(r.clone()).collect()).collect()
}

/// Inserts `clip` at frame `at`. A mono clip goes into every channel; extra clip channels are
/// dropped and missing ones repeat its last channel.
pub fn paste(a: &mut Planar, at: usize, clip: &Planar) {
    let Some(last) = clip.last() else {
        return;
    };
    let at = at.min(frames(a));
    for (i, c) in a.iter_mut().enumerate() {
        let src = clip.get(i).unwrap_or(last);
        c.splice(at..at, src.iter().copied());
    }
}

/// Sets `r` to silence.
pub fn silence(a: &mut Planar, r: Range<usize>) {
    let r = clamp(a, &r);
    for c in a.iter_mut() {
        c[r.clone()].fill(0.0);
    }
}

/// The largest absolute sample in `r`.
pub fn peak(a: &Planar, r: Range<usize>) -> f32 {
    let r = clamp(a, &r);
    a.iter()
        .flat_map(|c| c[r.clone()].iter())
        .fold(0.0_f32, |m, &v| m.max(v.abs()))
}

/// Multiplies `r` by `db` decibels.
pub fn gain(a: &mut Planar, r: Range<usize>, db: f32) {
    let g = 10f32.powf(db / 20.0);
    let r = clamp(a, &r);
    for c in a.iter_mut() {
        for v in &mut c[r.clone()] {
            *v *= g;
        }
    }
}

/// Scales `r` so its peak is `db` dBFS. Silence stays silent.
pub fn normalize(a: &mut Planar, r: Range<usize>, db: f32) {
    let p = peak(a, r.clone());
    if p > 0.0 {
        gain(a, r, db - 20.0 * p.log10());
    }
}

/// Fades `r` in (from silence) or out (to silence) with an equal-power curve.
pub fn fade(a: &mut Planar, r: Range<usize>, fade_in: bool) {
    let r = clamp(a, &r);
    let n = r.len();
    if n == 0 {
        return;
    }
    for c in a.iter_mut() {
        for (i, v) in c[r.clone()].iter_mut().enumerate() {
            // 0 at the silent end, 1 at the full end.
            let t = if fade_in {
                i as f32 / n as f32
            } else {
                1.0 - (i + 1) as f32 / n as f32
            };
            *v *= (t * std::f32::consts::FRAC_PI_2).sin();
        }
    }
}

/// Plays `r` backwards.
pub fn reverse(a: &mut Planar, r: Range<usize>) {
    let r = clamp(a, &r);
    for c in a.iter_mut() {
        c[r.clone()].reverse();
    }
}

/// Removes the DC offset of `r`: each channel's mean over `r` is subtracted.
pub fn remove_dc(a: &mut Planar, r: Range<usize>) {
    let r = clamp(a, &r);
    if r.is_empty() {
        return;
    }
    for c in a.iter_mut() {
        let mean = c[r.clone()].iter().map(|&v| f64::from(v)).sum::<f64>() / r.len() as f64;
        for v in &mut c[r.clone()] {
            *v -= mean as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(n: usize) -> Planar {
        vec![
            (0..n).map(|i| i as f32).collect(),
            (0..n).map(|i| -(i as f32)).collect(),
        ]
    }

    #[test]
    fn cut_copy_paste_and_trim_move_the_right_frames() {
        let mut a = ramp(10);
        assert_eq!(copy(&a, 2..5)[0], vec![2.0, 3.0, 4.0]);
        let c = cut(&mut a, 2..5);
        assert_eq!(c[1], vec![-2.0, -3.0, -4.0]);
        assert_eq!(a[0], vec![0.0, 1.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
        paste(&mut a, 2, &c);
        assert_eq!(a, ramp(10), "cut then paste back is the original");
        // A mono clip pastes into both channels; out-of-range positions clamp.
        paste(&mut a, 99, &vec![vec![7.0]]);
        assert_eq!((a[0][10], a[1][10]), (7.0, 7.0));
        trim(&mut a, 3..6);
        assert_eq!(a[0], vec![3.0, 4.0, 5.0]);
        #[allow(clippy::reversed_empty_ranges)]
        trim(&mut a, 5..2);
        assert!(a[0].is_empty());
    }

    #[test]
    fn level_edits_hit_their_targets() {
        let sine: Vec<f32> = (0..4800).map(|i| 0.25 * (i as f32 * 0.05).sin()).collect();
        let mut a = vec![sine.clone()];
        normalize(&mut a, 0..4800, -1.0);
        assert!((20.0 * peak(&a, 0..4800).log10() + 1.0).abs() < 1e-3);
        let mut b = vec![sine.clone()];
        gain(&mut b, 0..4800, -6.0206);
        assert!((peak(&b, 0..4800) - 0.5 * peak(&vec![sine.clone()], 0..4800)).abs() < 1e-4);
        silence(&mut b, 100..200);
        assert_eq!(peak(&b, 100..200), 0.0);
        // DC: a sine on a 0.3 offset ends up centred.
        let mut c = vec![sine.iter().map(|v| v + 0.3).collect::<Vec<_>>()];
        remove_dc(&mut c, 0..4800);
        let mean: f32 = c[0].iter().sum::<f32>() / 4800.0;
        assert!(mean.abs() < 1e-3, "{mean}");
        // Fades: silent at the outer end, untouched past the range.
        let mut d = vec![vec![1.0_f32; 100]];
        fade(&mut d, 0..50, true);
        assert_eq!(d[0][0], 0.0);
        assert!(d[0][49] > 0.99 && d[0][50] == 1.0);
        fade(&mut d, 50..100, false);
        assert!(d[0][99].abs() < 1e-6 && d[0][50] > 0.99);
        // Reverse twice is the original.
        let mut e = ramp(9);
        reverse(&mut e, 2..7);
        assert_eq!(e[0][2..7], [6.0, 5.0, 4.0, 3.0, 2.0]);
        reverse(&mut e, 2..7);
        assert_eq!(e, ramp(9));
    }
}
