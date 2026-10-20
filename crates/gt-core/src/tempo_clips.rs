//! Tempo automation (Prompt 31, D144).
//!
//! An automation clip whose target is [`ParamId::Tempo`] does not move a parameter while the
//! song plays: it becomes part of the tempo map. [`Project::song_tempo`] is the project's tempo
//! map with each such clip's span replaced by the clip's curve (its points become tempo points
//! whose ramps are the segments' curves), so the scheduler integrates it exactly like any other
//! ramp. Everything that plays or exports the song uses that map.

use crate::params::{ParamId, TEMPO_INFO};
use crate::playlist::{Clip, ClipKind, Curve};
use crate::project::Project;
use crate::time::{TempoMap, TempoPoint, Tick};

impl Project {
    /// The tempo clips that play: unmuted, on audible tracks, sorted by start.
    fn tempo_clips(&self) -> Vec<&Clip> {
        let pl = &self.playlist;
        let mut v: Vec<&Clip> = pl
            .clips
            .iter()
            .filter(|c| {
                matches!(&c.kind, ClipKind::Automation(a) if a.target == ParamId::Tempo)
                    && !c.muted
                    && pl.is_track_audible(c.track)
            })
            .collect();
        v.sort_by_key(|c| (c.start, c.id));
        v
    }

    /// The tempo map the song plays with: [`Project::tempo`] with the spans of tempo
    /// automation clips replaced by their curves. Where clips overlap, the later one takes
    /// over from its start. After a clip the project tempo resumes.
    pub fn song_tempo(&self) -> TempoMap {
        let clips = self.tempo_clips();
        if clips.is_empty() {
            return self.tempo.clone();
        }
        let base: Vec<TempoPoint> = self.tempo.points().collect();
        // Each clip covers [start, end) up to where the next one starts.
        let spans: Vec<(i64, i64)> = clips
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let end = clips.get(i + 1).map_or(c.end(), |n| n.start.min(c.end()));
                (c.start.max(0), end.max(c.start.max(0)))
            })
            .collect();
        let covered = |t: i64| spans.iter().any(|&(a, b)| a <= t && t < b);
        let mut out: Vec<TempoPoint> = base.iter().copied().filter(|p| !covered(p.at.0)).collect();
        // Where a span ends outside any other, the project tempo comes back: its value there,
        // ramping on to the next project point when it was ramping.
        for &(_, end) in &spans {
            if covered(end) {
                continue;
            }
            let ramp = base
                .iter()
                .rev()
                .find(|p| p.at.0 <= end)
                .map_or(Curve::Hold, |p| {
                    if p.ramp == Curve::Hold {
                        Curve::Hold
                    } else {
                        Curve::Linear
                    }
                });
            out.push(TempoPoint {
                at: Tick(end),
                bpm: self.tempo.bpm_at(Tick(end)),
                ramp,
            });
        }
        let bpm = |v: f32| f64::from(TEMPO_INFO.from_normalized(v));
        for (c, &(start, end)) in clips.iter().zip(&spans) {
            let ClipKind::Automation(a) = &c.kind else {
                continue;
            };
            if end <= start || a.points.is_empty() {
                continue;
            }
            // Source position at timeline tick `start`.
            let src = c.offset + (start - c.start);
            // The segment the clip opens in decides how it moves from its first value.
            let ramp = a
                .points
                .iter()
                .rev()
                .find(|p| p.at <= src)
                .filter(|p| a.points.last().is_some_and(|l| l.at > p.at))
                .map_or(Curve::Hold, |p| p.curve);
            out.push(TempoPoint {
                at: Tick(start),
                bpm: bpm(a.value_at(src as f64)),
                ramp,
            });
            let last = a.points.len() - 1;
            for (i, p) in a.points.iter().enumerate() {
                let at = c.start + (p.at - c.offset);
                if at <= start || at >= end {
                    continue;
                }
                out.push(TempoPoint {
                    at: Tick(at),
                    bpm: bpm(p.value),
                    // After the last point its value holds.
                    ramp: if i == last { Curve::Hold } else { p.curve },
                });
            }
        }
        TempoMap::from_points_lossy(&out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playlist::{AutoPoint, Automation};
    use crate::PPQ;

    fn tempo_clip(p: &mut Project, start: i64, len: i64, points: Vec<AutoPoint>) {
        let t = p.playlist.tracks[0].id;
        p.playlist.add_clip(
            t,
            start,
            len,
            ClipKind::Automation(Automation {
                target: ParamId::Tempo,
                points,
            }),
        );
    }

    #[test]
    fn a_tempo_clip_ramps_the_song_tempo_and_the_project_tempo_resumes() {
        let mut p = Project::empty();
        p.tempo = TempoMap::constant(100.0);
        let n = |bpm: f32| TEMPO_INFO.to_normalized(bpm);
        // Bars 2 to 4: a linear ramp from 120 to 160 over two bars, then held.
        tempo_clip(
            &mut p,
            3840,
            3 * 3840,
            vec![
                AutoPoint::new(0, n(120.0)),
                AutoPoint::new(2 * 3840, n(160.0)),
            ],
        );
        let m = p.song_tempo();
        assert_eq!(m.bpm_at(Tick(0)), 100.0);
        assert!((m.bpm_at(Tick(3840)) - 120.0).abs() < 1e-3);
        assert!(
            (m.bpm_at(Tick(2 * 3840)) - 140.0).abs() < 1e-3,
            "half way up the ramp"
        );
        assert!((m.bpm_at(Tick(3 * 3840 + PPQ)) - 160.0).abs() < 1e-3);
        assert_eq!(m.bpm_at(Tick(4 * 3840)), 100.0, "after the clip");
        assert!(m.has_ramps());
        // A muted clip, or no clip, leaves the project tempo.
        p.playlist.clips[0].muted = true;
        assert_eq!(p.song_tempo(), p.tempo);
        // The project's own tempo map is untouched.
        assert!(!p.tempo.has_ramps());
    }
}
