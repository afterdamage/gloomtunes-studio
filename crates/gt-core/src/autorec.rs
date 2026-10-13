//! Recorded automation (Prompt 17, D111): thinning a pass of control moves into a few
//! editable points, and writing a pass into the playlist's automation clips.

use crate::params::ParamId;
use crate::playlist::{value_at, AutoPoint, Automation, ClipId, ClipKind};
use crate::project::Project;

/// Default thinning tolerance: the largest vertical error (normalized value) a dropped point
/// may leave, about half a percent of the range.
pub const THIN_TOLERANCE: f32 = 0.004;

/// Drops points that a straight line between their neighbours already reproduces within
/// `tol` (Ramer–Douglas–Peucker on the vertical error). The first and last points stay, and
/// the line through the kept points never strays more than `tol` from any input point.
/// `points` must be sorted by `at`.
pub fn thin(points: &[AutoPoint], tol: f32) -> Vec<AutoPoint> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0, points.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (pa, pb) = (points[a], points[b]);
        let span = (pb.at - pa.at) as f32;
        let line = |at: i64| {
            if span <= 0.0 {
                pb.value
            } else {
                pa.value + (pb.value - pa.value) * ((at - pa.at) as f32 / span)
            }
        };
        let (worst, err) = (a + 1..b)
            .map(|i| (i, (points[i].value - line(points[i].at)).abs()))
            .fold((a, -1.0f32), |m, x| if x.1 > m.1 { x } else { m });
        if err > tol {
            keep[worst] = true;
            stack.push((a, worst));
            stack.push((worst, b));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(*p))
        .collect()
}

/// Writes a recorded pass (points at song ticks, sorted, values normalized) for `target` into
/// the playlist and returns the clip that holds it.
///
/// An automation clip for `target` that covers (or ends at) the pass start takes it: the clip
/// grows to the pass end if its track is free there, and its points inside the pass are
/// replaced. Guard points just outside the pass keep the old line on either side, so a touch
/// pass returns to the automation that was there. Without such a clip a new one is made on a
/// track already holding `target` automation, else the first free track, else a new track.
pub fn record_automation(
    project: &mut Project,
    target: ParamId,
    pass: &[AutoPoint],
) -> Option<ClipId> {
    let (first, last) = (pass.first()?, pass.last()?);
    let (t0, t1) = (first.at, last.at.max(first.at));
    let name = target.name(project);
    let pl = &mut project.playlist;
    let is_target = |k: &ClipKind| matches!(k, ClipKind::Automation(a) if a.target == target);
    let found = pl
        .clips
        .iter()
        .filter(|c| !c.muted && is_target(&c.kind))
        .find(|c| c.start <= t0 && t0 < c.end())
        .or_else(|| {
            pl.clips
                .iter()
                .find(|c| !c.muted && is_target(&c.kind) && c.end() == t0)
        })
        .map(|c| c.id);
    let Some(id) = found else {
        let free = |t| {
            !pl.clips
                .iter()
                .any(|c| c.track == t && c.start <= t1 && c.end() > t0)
        };
        let track = pl
            .clips
            .iter()
            .filter(|c| is_target(&c.kind))
            .map(|c| c.track)
            .find(|&t| free(t))
            .or_else(|| pl.tracks.iter().map(|t| t.id).find(|&t| free(t)))
            .unwrap_or_else(|| pl.add_track());
        let empty = !pl.clips.iter().any(|c| c.track == track);
        if let Some(t) = pl.track_mut(track) {
            if empty && t.name.starts_with("Track ") {
                t.name = name;
            }
        }
        let points = pass
            .iter()
            .map(|p| AutoPoint {
                at: p.at - t0,
                value: p.value.clamp(0.0, 1.0),
                ..*p
            })
            .collect();
        return Some(pl.add_clip(
            track,
            t0,
            t1 - t0 + 1,
            ClipKind::Automation(Automation { target, points }),
        ));
    };
    let c = pl.clip(id)?;
    let (start, track, offset) = (c.start, c.track, c.offset);
    // The clip may grow up to the next clip on its track.
    let limit = pl
        .clips
        .iter()
        .filter(|o| o.id != id && o.track == track && o.start >= c.end())
        .map(|o| o.start)
        .min()
        .unwrap_or(i64::MAX);
    let end = c.end().max((t1 + 1).min(limit));
    let src = |t: i64| t - start + offset;
    let clip = pl.clip_mut(id)?;
    clip.length = end - start;
    let ClipKind::Automation(a) = &mut clip.kind else {
        return None;
    };
    let new: Vec<AutoPoint> = pass
        .iter()
        .filter(|p| p.at < end)
        .map(|p| AutoPoint {
            at: src(p.at),
            value: p.value.clamp(0.0, 1.0),
            ..*p
        })
        .collect();
    let (s0, s1) = (src(t0), new.last().map_or(src(t0), |p| p.at));
    let old = std::mem::take(&mut a.points);
    let mut points: Vec<AutoPoint> = old.iter().copied().filter(|p| p.at < s0).collect();
    if let Some(prev) = points.last() {
        if prev.at < s0 - 1 {
            let v = value_at(&old, (s0 - 1) as f64);
            points.push(AutoPoint {
                at: s0 - 1,
                value: v,
                curve: crate::Curve::Linear,
            });
        }
    }
    points.extend(new);
    let after: Vec<AutoPoint> = old.iter().copied().filter(|p| p.at > s1).collect();
    if let Some(next) = after.first() {
        if next.at > s1 + 1 {
            let v = value_at(&old, (s1 + 1) as f64);
            points.push(AutoPoint {
                at: s1 + 1,
                value: v,
                curve: crate::Curve::Linear,
            });
        }
    }
    points.extend(after);
    a.points = points;
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PPQ;

    fn pts(v: &[(i64, f32)]) -> Vec<AutoPoint> {
        v.iter()
            .map(|&(at, value)| AutoPoint::new(at, value))
            .collect()
    }

    #[test]
    fn thinning_keeps_the_shape_within_the_tolerance() {
        // A slow sine sampled every 10 ticks over four beats.
        let dense: Vec<AutoPoint> = (0..=384)
            .map(|i| {
                let at = i * 10;
                let x = at as f32 / (4 * PPQ) as f32;
                AutoPoint::new(at, 0.5 + 0.4 * (x * std::f32::consts::TAU).sin())
            })
            .collect();
        let thin_pts = thin(&dense, THIN_TOLERANCE);
        assert!(
            thin_pts.len() < dense.len() / 8,
            "{} points",
            thin_pts.len()
        );
        assert_eq!(thin_pts.first(), dense.first());
        assert_eq!(thin_pts.last(), dense.last());
        for p in &dense {
            let err = (value_at(&thin_pts, p.at as f64) - p.value).abs();
            assert!(err <= THIN_TOLERANCE + 1e-6, "error {err} at {}", p.at);
        }
        // A straight ramp needs only its ends; a step keeps its corners.
        let ramp = pts(&[(0, 0.0), (10, 0.1), (20, 0.2), (30, 0.3)]);
        assert_eq!(thin(&ramp, THIN_TOLERANCE), pts(&[(0, 0.0), (30, 0.3)]));
        let step = pts(&[(0, 0.2), (10, 0.2), (11, 0.8), (20, 0.8)]);
        assert_eq!(thin(&step, THIN_TOLERANCE), step);
    }

    fn auto_clips(p: &Project, target: ParamId) -> Vec<(i64, i64, Vec<AutoPoint>)> {
        p.playlist
            .clips
            .iter()
            .filter_map(|c| match &c.kind {
                ClipKind::Automation(a) if a.target == target => {
                    Some((c.start, c.length, a.points.clone()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_pass_without_a_clip_makes_one_on_a_free_named_track() {
        let mut p = Project::empty();
        let target = ParamId::strip_pan(2);
        let id = record_automation(&mut p, target, &pts(&[(960, 0.5), (1920, 0.9)])).unwrap();
        let c = p.playlist.clip(id).unwrap();
        assert_eq!((c.start, c.length), (960, 961));
        assert_eq!(auto_clips(&p, target)[0].2, pts(&[(0, 0.5), (960, 0.9)]));
        assert_eq!(p.playlist.track(c.track).unwrap().name, target.name(&p));
    }

    #[test]
    fn a_pass_over_a_clip_replaces_its_points_and_keeps_the_line_outside() {
        let mut p = Project::empty();
        let target = ParamId::strip_pan(2);
        let track = p.playlist.tracks[0].id;
        let id = p.playlist.add_clip(
            track,
            0,
            4 * PPQ,
            ClipKind::Automation(Automation {
                target,
                points: pts(&[(0, 0.0), (4 * PPQ, 1.0)]),
            }),
        );
        let got = record_automation(&mut p, target, &pts(&[(960, 0.9), (1440, 0.1)])).unwrap();
        assert_eq!(got, id);
        let pts_now = &auto_clips(&p, target)[0].2;
        assert_eq!(
            *pts_now,
            pts(&[
                (0, 0.0),
                (959, 0.249_739_58),
                (960, 0.9),
                (1440, 0.1),
                (1441, 0.375_260_4),
                (3840, 1.0)
            ])
        );
        // A pass running past the clip end grows the clip.
        record_automation(&mut p, target, &pts(&[(3000, 0.5), (5000, 0.5)])).unwrap();
        let (start, len, pts_now) = auto_clips(&p, target).remove(0);
        assert_eq!((start, len), (0, 5001));
        assert_eq!(pts_now.last().unwrap().at, 5000);
    }
}
