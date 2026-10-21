//! Polyphonic automation (Prompt 34, D152): one expression curve carried by a note, played
//! on that note's voice alone as a note expression (tuning, pressure or timbre).
//!
//! The curve's points sit at fractions of the note's length, so it moves, stretches and copies
//! with the note. Values are 0 to 1; tuning is ±[`CURVE_TUNING_SEMIS`] around 0.5.

/// Most points on one curve.
pub const CURVE_POINTS: usize = 8;
/// Tuning range of a tuning curve, semitones either way.
pub const CURVE_TUNING_SEMIS: f32 = 12.0;

/// What a note's curve moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum CurveKind {
    /// No curve.
    #[default]
    Off,
    /// The note's pitch.
    Tuning,
    /// The note's pressure (polyphonic aftertouch).
    Pressure,
    /// The note's timbre (mod X on built-in instruments, MPE timbre on plugins).
    Timbre,
}

impl CurveKind {
    /// The kinds a curve can have, in menu order.
    pub const ALL: [CurveKind; 3] = [Self::Tuning, Self::Pressure, Self::Timbre];

    /// Menu label.
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Tuning => "Tuning",
            Self::Pressure => "Pressure",
            Self::Timbre => "Timbre",
        }
    }

    /// Project-file key.
    pub fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Tuning => "tuning",
            Self::Pressure => "pressure",
            Self::Timbre => "timbre",
        }
    }

    /// The kind for a key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key() == key)
    }

    /// The value where the curve leaves the note as it is.
    pub fn neutral(self) -> f32 {
        match self {
            Self::Pressure => 0.0,
            _ => 0.5,
        }
    }
}

/// One point: where in the note (0 to 1 of its length) and the value (0 to 1).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CurvePoint {
    /// Position, 0 (note start) to 1 (note end).
    pub at: f32,
    /// Value, 0 to 1.
    pub value: f32,
}

/// A note's expression curve: straight lines between points sorted by position, holding the
/// first and last values outside them.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct NoteCurve {
    /// What it moves; `Off` for none.
    pub kind: CurveKind,
    len: u8,
    points: [CurvePoint; CURVE_POINTS],
}

impl NoteCurve {
    /// A curve of `kind` through `points` (sorted, clamped, the first [`CURVE_POINTS`]).
    pub fn new(kind: CurveKind, points: &[CurvePoint]) -> Self {
        let mut c = Self {
            kind,
            ..Self::default()
        };
        for p in points.iter().take(CURVE_POINTS) {
            c.points[usize::from(c.len)] = *p;
            c.len += 1;
        }
        c.sanitize();
        c
    }

    /// True when the curve does nothing.
    pub fn is_off(&self) -> bool {
        self.kind == CurveKind::Off || self.len == 0
    }

    /// The points.
    pub fn points(&self) -> &[CurvePoint] {
        &self.points[..usize::from(self.len)]
    }

    /// Value at `x` (0 to 1 of the note).
    pub fn value_at(&self, x: f32) -> f32 {
        let pts = self.points();
        let Some(first) = pts.first() else {
            return self.kind.neutral();
        };
        if x <= first.at {
            return first.value;
        }
        for w in pts.windows(2) {
            if x <= w[1].at {
                let span = w[1].at - w[0].at;
                if span <= 0.0 {
                    return w[1].value;
                }
                return w[0].value + (w[1].value - w[0].value) * (x - w[0].at) / span;
            }
        }
        pts[pts.len() - 1].value
    }

    /// Sets the value at `at`: moves a point within `near` of it, else adds one (when full, the
    /// nearest point moves instead).
    pub fn set_point(&mut self, at: f32, value: f32, near: f32) {
        let (at, value) = (at.clamp(0.0, 1.0), value.clamp(0.0, 1.0));
        let n = usize::from(self.len);
        let nearest = (0..n).min_by(|&a, &b| {
            (self.points[a].at - at)
                .abs()
                .total_cmp(&(self.points[b].at - at).abs())
        });
        match nearest {
            Some(i) if (self.points[i].at - at).abs() <= near || n == CURVE_POINTS => {
                self.points[i] = CurvePoint { at, value };
            }
            _ => {
                self.points[n] = CurvePoint { at, value };
                self.len += 1;
            }
        }
        self.sanitize();
    }

    /// Removes the point nearest `at` if within `near`; the curve turns off with its last point.
    pub fn remove_near(&mut self, at: f32, near: f32) -> bool {
        let n = usize::from(self.len);
        let Some(i) = (0..n)
            .filter(|&i| (self.points[i].at - at).abs() <= near)
            .min_by(|&a, &b| {
                (self.points[a].at - at)
                    .abs()
                    .total_cmp(&(self.points[b].at - at).abs())
            })
        else {
            return false;
        };
        self.points.copy_within(i + 1..n, i);
        self.len -= 1;
        if self.len == 0 {
            self.kind = CurveKind::Off;
        }
        true
    }

    /// Clamps and sorts the points; non-numbers become neutral.
    pub fn sanitize(&mut self) {
        let neutral = self.kind.neutral();
        let n = usize::from(self.len).min(CURVE_POINTS);
        self.len = n as u8;
        for p in &mut self.points[..n] {
            p.at = if p.at.is_finite() {
                p.at.clamp(0.0, 1.0)
            } else {
                0.0
            };
            p.value = if p.value.is_finite() {
                p.value.clamp(0.0, 1.0)
            } else {
                neutral
            };
        }
        self.points[..n].sort_by(|a, b| a.at.total_cmp(&b.at));
        for p in &mut self.points[n..] {
            *p = CurvePoint::default();
        }
        if self.kind == CurveKind::Off {
            self.len = 0;
            self.points = [CurvePoint::default(); CURVE_POINTS];
        }
    }

    /// The engine value of a curve value: semitones for tuning, 0 to 1 otherwise.
    pub fn plain(kind: CurveKind, v: f32) -> f32 {
        match kind {
            CurveKind::Tuning => (v - 0.5) * 2.0 * CURVE_TUNING_SEMIS,
            _ => v,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(at: f32, value: f32) -> CurvePoint {
        CurvePoint { at, value }
    }

    #[test]
    fn values_between_and_outside_points() {
        let c = NoteCurve::new(CurveKind::Timbre, &[pt(0.5, 1.0), pt(0.0, 0.0)]);
        assert_eq!(c.points()[0].at, 0.0, "sorted");
        assert_eq!(c.value_at(0.25), 0.5);
        assert_eq!(c.value_at(0.9), 1.0);
        assert_eq!(NoteCurve::default().value_at(0.3), 0.5);
        assert!(NoteCurve::default().is_off());
        assert_eq!(NoteCurve::plain(CurveKind::Tuning, 1.0), 12.0);
        assert_eq!(NoteCurve::plain(CurveKind::Tuning, 0.5), 0.0);
    }

    #[test]
    fn editing_points() {
        let mut c = NoteCurve::new(CurveKind::Pressure, &[]);
        c.set_point(0.2, 0.4, 0.02);
        c.set_point(0.21, 0.6, 0.02);
        assert_eq!(c.points(), &[pt(0.21, 0.6)], "moved, not added");
        for k in 0..20 {
            c.set_point(k as f32 / 20.0, 0.5, 0.01);
        }
        assert_eq!(c.points().len(), CURVE_POINTS);
        assert!(c.points().windows(2).all(|w| w[0].at <= w[1].at));
        while let Some(q) = c.points().first().copied() {
            assert!(c.remove_near(q.at, 0.01));
        }
        assert!(c.is_off());
        assert_eq!(c.kind, CurveKind::Off);
        let mut bad = NoteCurve::new(CurveKind::Tuning, &[pt(f32::NAN, f32::NAN), pt(3.0, 2.0)]);
        bad.sanitize();
        assert_eq!(bad.points(), &[pt(0.0, 0.5), pt(1.0, 1.0)]);
    }
}
