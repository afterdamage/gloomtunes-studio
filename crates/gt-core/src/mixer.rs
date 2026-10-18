//! The mixer: a master strip and a variable number of insert strips (Prompt 29).
//!
//! Strips are addressed by index: 0 is the master, 1 and up are inserts. Channels feed a strip.
//! Every insert has an output (another strip) and any number of sends (up to [`MAX_SENDS`]) to
//! other strips, each with a level and pre- or post-fader; any strip can take another strip's
//! signal as a compressor sidechain key. Together these form the routing graph, which must be
//! free of cycles; [`Mixer::processing_order`] sorts it.
//!
//! Before Prompt 29 the mixer had 64 inserts and 4 send buses (strips 65 to 68) with a fixed
//! send level per insert into each bus; old files load as 68 inserts, the buses as ordinary
//! inserts and each level as a post-fader send to them (D141).

use crate::effects::EffectSlot;
use crate::recording::AudioInput;

/// Most insert strips.
pub const MAX_INSERTS: usize = 256;
/// Inserts in a new project.
pub const DEFAULT_INSERTS: usize = 64;
/// Most strips: the master and [`MAX_INSERTS`] inserts.
pub const STRIPS: usize = 1 + MAX_INSERTS;
/// Most sends per strip.
pub const MAX_SENDS: usize = 8;
/// Effect slots per strip.
pub const FX_SLOTS: usize = 10;
/// Index of the master strip.
pub const MASTER: usize = 0;

/// Insert the demo projects use as their reverb return (where the first send bus was).
pub const DEMO_REVERB: usize = 65;
/// Insert the demo projects use as their delay return.
pub const DEMO_DELAY: usize = 66;

/// What a strip index is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripKind {
    /// The master strip, which feeds the audio device.
    Master,
    /// Insert strip, numbered from 1.
    Insert(usize),
}

impl StripKind {
    /// Kind of strip `index`.
    pub fn of(index: usize) -> Self {
        match index {
            MASTER => Self::Master,
            i => Self::Insert(i),
        }
    }

    /// Short label: "M", "3".
    pub fn short(self) -> String {
        match self {
            Self::Master => "M".to_owned(),
            Self::Insert(n) => n.to_string(),
        }
    }

    /// Default name: "Master", "Insert 3".
    pub fn default_name(self) -> String {
        match self {
            Self::Master => "Master".to_owned(),
            Self::Insert(n) => format!("Insert {n}"),
        }
    }
}

/// One send from a strip to another.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Send {
    /// Strip it feeds.
    pub to: usize,
    /// Level, linear 0 to 1.
    pub level: f32,
    /// Taken before the fader (after the effects) instead of after it.
    pub pre: bool,
}

/// How a strip's pan knob splits the signal (Prompt 29).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanLaw {
    /// Centre is unity on both sides; turning one way lowers the other side only (a balance,
    /// the behaviour before Prompt 29).
    #[default]
    Zero,
    /// Constant power: -3 dB per side at centre.
    Minus3,
    /// Between constant power and linear: -4.5 dB at centre.
    Minus4_5,
    /// Linear: -6 dB per side at centre.
    Minus6,
}

impl PanLaw {
    /// Every law.
    pub const ALL: [PanLaw; 4] = [Self::Zero, Self::Minus3, Self::Minus4_5, Self::Minus6];

    /// Label, e.g. "-3 dB".
    pub fn label(self) -> &'static str {
        match self {
            Self::Zero => "0 dB",
            Self::Minus3 => "-3 dB",
            Self::Minus4_5 => "-4.5 dB",
            Self::Minus6 => "-6 dB",
        }
    }

    /// Name in files.
    pub fn key(self) -> &'static str {
        match self {
            Self::Zero => "0",
            Self::Minus3 => "-3",
            Self::Minus4_5 => "-4.5",
            Self::Minus6 => "-6",
        }
    }

    /// The law named `key`.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.key() == key)
    }

    /// Left and right gains for `pan` (-1 to 1). Fully to one side, that side is unity.
    pub fn gains(self, pan: f32) -> (f32, f32) {
        let p = if pan.is_finite() {
            pan.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let linear = ((1.0 - p) * 0.5, (1.0 + p) * 0.5);
        let angle = (p + 1.0) * core::f32::consts::FRAC_PI_4;
        let power = (angle.cos(), angle.sin());
        match self {
            Self::Zero => (1.0 - p.max(0.0), 1.0 + p.min(0.0)),
            Self::Minus3 => power,
            Self::Minus4_5 => ((power.0 * linear.0).sqrt(), (power.1 * linear.1).sqrt()),
            Self::Minus6 => linear,
        }
    }
}

/// One mixer strip.
#[derive(Debug, Clone, PartialEq)]
pub struct MixerStrip {
    /// Display name.
    pub name: String,
    /// Fader gain, linear, 0 to [`MixerStrip::MAX_VOLUME`].
    pub volume: f32,
    /// Pan from -1 to 1.
    pub pan: f32,
    /// Muted.
    pub mute: bool,
    /// Soloed (see [`Mixer::audible`]).
    pub solo: bool,
    /// Flip the polarity.
    pub phase_invert: bool,
    /// Strip this one feeds (ignored on the master).
    pub output: usize,
    /// Sends to other strips, at most [`MAX_SENDS`], one per target (inserts only).
    pub sends: Vec<Send>,
    /// Strip whose output is the key for this strip's compressors' sidechain input.
    pub sidechain: Option<usize>,
    /// Effects, processed top to bottom.
    pub slots: [Option<EffectSlot>; FX_SLOTS],
    /// A device input heard live through this strip (inserts only), for live effects.
    pub input: Option<AudioInput>,
}

impl MixerStrip {
    /// Fader maximum: +6 dB.
    pub const MAX_VOLUME: f32 = 1.995_262_3;
    /// New strips sit at unity, so routing a channel through one changes nothing.
    pub const DEFAULT_VOLUME: f32 = 1.0;

    /// A strip at unity into the master, named for its index.
    pub fn new(index: usize) -> Self {
        Self {
            name: StripKind::of(index).default_name(),
            volume: Self::DEFAULT_VOLUME,
            pan: 0.0,
            mute: false,
            solo: false,
            phase_invert: false,
            output: MASTER,
            sends: Vec::new(),
            sidechain: None,
            slots: Default::default(),
            input: None,
        }
    }

    /// Level of the send to `to` (0 when there is none).
    pub fn send_level(&self, to: usize) -> f32 {
        self.sends
            .iter()
            .find(|s| s.to == to)
            .map_or(0.0, |s| s.level)
    }

    /// The send to `to`, if there is one.
    pub fn send_mut(&mut self, to: usize) -> Option<&mut Send> {
        self.sends.iter_mut().find(|s| s.to == to)
    }
}

/// All strips: the master first, then the inserts.
#[derive(Debug, Clone, PartialEq)]
pub struct Mixer {
    /// Strips by index, 2 to [`STRIPS`] long.
    pub strips: Vec<MixerStrip>,
    /// How pan knobs split the signal.
    pub pan_law: PanLaw,
}

impl Default for Mixer {
    fn default() -> Self {
        Self::new()
    }
}

impl Mixer {
    /// The master and [`DEFAULT_INSERTS`] inserts, every strip at unity into the master.
    pub fn new() -> Self {
        Self::with_inserts(DEFAULT_INSERTS)
    }

    /// The master and `n` inserts (1 to [`MAX_INSERTS`]).
    pub fn with_inserts(n: usize) -> Self {
        Self {
            strips: (0..=n.clamp(1, MAX_INSERTS)).map(MixerStrip::new).collect(),
            pan_law: PanLaw::Zero,
        }
    }

    /// Number of inserts.
    pub fn inserts(&self) -> usize {
        self.strips.len() - 1
    }

    /// Appends an insert and returns its index; `None` when there are [`MAX_INSERTS`].
    pub fn add_insert(&mut self) -> Option<usize> {
        if self.strips.len() >= STRIPS {
            return None;
        }
        let i = self.strips.len();
        self.strips.push(MixerStrip::new(i));
        Some(i)
    }

    /// Makes sure inserts [`DEMO_REVERB`] and [`DEMO_DELAY`] exist (adding plain inserts up to
    /// them) and returns them.
    pub fn fx_returns(&mut self) -> (usize, usize) {
        while self.strips.len() <= DEMO_DELAY {
            self.add_insert();
        }
        (DEMO_REVERB, DEMO_DELAY)
    }

    /// Removes insert `i`; strips after it move down one. Routing into it goes to the master,
    /// sends and sidechains from it are dropped. [`Mixer::remap`] gives every strip index's new
    /// value, for the project's own references. False if `i` is the master,
    /// out of range or the last insert.
    pub fn remove_insert(&mut self, i: usize) -> bool {
        if i == MASTER || i >= self.strips.len() || self.inserts() <= 1 {
            return false;
        }
        self.strips.remove(i);
        for s in &mut self.strips {
            s.output = Self::remap(i, s.output).unwrap_or(MASTER);
            s.sends.retain_mut(|x| match Self::remap(i, x.to) {
                Some(t) => {
                    x.to = t;
                    true
                }
                None => false,
            });
            s.sidechain = s.sidechain.and_then(|k| Self::remap(i, k));
        }
        true
    }

    /// Where strip `index` moves when insert `removed` is deleted (`None`: it is the one).
    pub fn remap(removed: usize, index: usize) -> Option<usize> {
        match index.cmp(&removed) {
            std::cmp::Ordering::Less => Some(index),
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Greater => Some(index - 1),
        }
    }

    /// Routing edges `(from, to)`: outputs, sends (whatever their level, so turning one up
    /// can never close a loop) and sidechain keys.
    fn edges(&self) -> Vec<(usize, usize)> {
        let mut e = Vec::new();
        for (i, s) in self.strips.iter().enumerate() {
            if i != MASTER {
                e.push((i, s.output));
                e.extend(s.sends.iter().map(|x| (i, x.to)));
            }
            if let Some(src) = s.sidechain {
                e.push((src, i));
            }
        }
        e
    }

    /// True if `to` can be reached from `from` along `edges`.
    fn reaches(&self, edges: &[(usize, usize)], from: usize, to: usize) -> bool {
        let mut seen = vec![false; self.strips.len()];
        let mut stack = vec![from];
        while let Some(n) = stack.pop() {
            if n == to {
                return true;
            }
            if n >= seen.len() || std::mem::replace(&mut seen[n], true) {
                continue;
            }
            stack.extend(edges.iter().filter(|e| e.0 == n).map(|e| e.1));
        }
        false
    }

    fn valid_pair(&self, strip: usize, target: usize) -> bool {
        strip != MASTER
            && strip < self.strips.len()
            && target < self.strips.len()
            && strip != target
    }

    /// Whether `strip` may output to `target` without closing a loop.
    pub fn can_route(&self, strip: usize, target: usize) -> bool {
        if !self.valid_pair(strip, target) {
            return false;
        }
        let out = self.strips[strip].output;
        let edges: Vec<_> = self
            .edges()
            .into_iter()
            .filter(|e| e.0 != strip || e.1 != out)
            .collect();
        !self.reaches(&edges, target, strip)
    }

    /// Whether `strip` may send to `target` (a new send, or moving no existing one) without
    /// closing a loop.
    pub fn can_send(&self, strip: usize, target: usize) -> bool {
        self.valid_pair(strip, target) && !self.reaches(&self.edges(), target, strip)
    }

    /// Adds (or keeps) a send from `strip` to `target`; false if that is not allowed or the
    /// strip has [`MAX_SENDS`] already.
    pub fn add_send(&mut self, strip: usize, target: usize, level: f32, pre: bool) -> bool {
        if self
            .strips
            .get(strip)
            .is_some_and(|s| s.sends.iter().any(|x| x.to == target))
        {
            return true;
        }
        if !self.can_send(strip, target) || self.strips[strip].sends.len() >= MAX_SENDS {
            return false;
        }
        self.strips[strip].sends.push(Send {
            to: target,
            level: level.clamp(0.0, 1.0),
            pre,
        });
        true
    }

    /// Whether `src` may be the sidechain key of `strip` without closing a loop.
    pub fn can_sidechain(&self, strip: usize, src: usize) -> bool {
        if src == MASTER || src >= self.strips.len() || strip >= self.strips.len() || src == strip {
            return false;
        }
        let edges: Vec<_> = self
            .edges()
            .into_iter()
            .filter(|&e| Some(e.0) != self.strips[strip].sidechain || e.1 != strip)
            .collect();
        !self.reaches(&edges, strip, src)
    }

    /// A processing order in which every strip comes after all strips that feed it, with the
    /// master last. `None` if the routing has a cycle.
    pub fn processing_order(&self) -> Option<Vec<usize>> {
        let n = self.strips.len();
        let edges = self.edges();
        let mut indegree = vec![0_usize; n];
        for &(_, to) in &edges {
            indegree[to] += 1;
        }
        // Kahn's algorithm; lowest index first keeps the order stable and readable.
        let mut ready: Vec<usize> = (1..n).rev().filter(|&i| indegree[i] == 0).collect();
        let mut order = Vec::with_capacity(n);
        while let Some(s) = ready.pop() {
            order.push(s);
            for &(from, to) in &edges {
                if from == s {
                    indegree[to] -= 1;
                    if indegree[to] == 0 && to != MASTER {
                        ready.push(to);
                        ready.sort_unstable_by(|a, b| b.cmp(a));
                    }
                }
            }
        }
        if order.len() != n - 1 || indegree[MASTER] != 0 {
            return None;
        }
        order.push(MASTER);
        Some(order)
    }

    /// Which strips are heard, after mute and solo. When any strip is soloed, a strip is
    /// heard only if it is soloed or lies on a path into or out of a soloed strip (so a soloed
    /// insert still reaches the master through its route and keeps its reverb send). The
    /// master is never silenced by solo. A muted strip is always silent.
    pub fn audible(&self) -> Vec<bool> {
        let n = self.strips.len();
        let mut out = vec![true; n];
        let soloed: Vec<usize> = (1..n).filter(|&i| self.strips[i].solo).collect();
        if !soloed.is_empty() {
            let edges = self.edges();
            for (i, o) in out.iter_mut().enumerate().skip(1) {
                *o = soloed
                    .iter()
                    .any(|&s| s == i || self.reaches(&edges, i, s) || self.reaches(&edges, s, i));
            }
        }
        for (o, s) in out.iter_mut().zip(&self.strips) {
            *o &= !s.mute;
        }
        out
    }

    /// Repairs a mixer read from a file or produced by a bug: strip count, ranges, and any
    /// routing that is invalid or cyclic (outputs reset to the master, sends dropped).
    pub fn sanitize(&mut self) {
        self.strips.truncate(STRIPS);
        while self.strips.len() < 2 {
            self.strips.push(MixerStrip::new(self.strips.len()));
        }
        let n = self.strips.len();
        for (i, s) in self.strips.iter_mut().enumerate() {
            s.volume = if s.volume.is_finite() {
                s.volume.clamp(0.0, MixerStrip::MAX_VOLUME)
            } else {
                MixerStrip::DEFAULT_VOLUME
            };
            s.pan = if s.pan.is_finite() {
                s.pan.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            if i == MASTER {
                s.sends.clear();
            }
            let mut seen = Vec::new();
            s.sends.retain_mut(|x| {
                x.level = if x.level.is_finite() {
                    x.level.clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let ok = x.to < n && x.to != i && !seen.contains(&x.to);
                seen.push(x.to);
                ok
            });
            s.sends.truncate(MAX_SENDS);
            if s.output >= n || s.output == i || i == MASTER {
                s.output = MASTER;
            }
            if i == MASTER || s.input.is_some_and(|x| !x.fits(crate::MAX_INPUT_CHANNELS)) {
                s.input = None;
            }
            if s.sidechain.is_some_and(|x| x >= n || x == i || x == MASTER) {
                s.sidechain = None;
            }
            for slot in s.slots.iter_mut() {
                if let Some(fx) = slot {
                    fx.sanitize();
                }
                // A plugin slot whose plugin is gone is an empty slot.
                if slot
                    .as_ref()
                    .is_some_and(|fx| fx.kind == crate::EffectKind::Plugin && fx.plugin.is_none())
                {
                    *slot = None;
                }
            }
        }
        for i in 1..n {
            let target = self.strips[i].output;
            if target != MASTER && !self.can_route(i, target) {
                self.strips[i].output = MASTER;
            }
            // Sends that close a loop go, last first.
            while self.processing_order().is_none() && !self.strips[i].sends.is_empty() {
                let probe = {
                    let mut m = self.clone();
                    m.strips[i].sends.clear();
                    m.processing_order().is_some()
                };
                if !probe {
                    break;
                }
                self.strips[i].sends.pop();
            }
            if let Some(src) = self.strips[i].sidechain {
                if !self.can_sidechain(i, src) {
                    self.strips[i].sidechain = None;
                }
            }
        }
        // Whatever loop is left (sends of several strips together) loses every send.
        if self.processing_order().is_none() {
            for s in &mut self.strips {
                s.sends.clear();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_order_has_master_last() {
        let m = Mixer::new();
        let order = m.processing_order().unwrap();
        assert_eq!(order.len(), 1 + DEFAULT_INSERTS);
        assert_eq!(*order.last().unwrap(), MASTER);
        assert_eq!(StripKind::of(3).short(), "3");
    }

    #[test]
    fn order_respects_routing_and_rejects_cycles() {
        let mut m = Mixer::with_inserts(12);
        m.strips[1].output = 5;
        assert!(m.add_send(5, 10, 0.5, false));
        assert!(m.add_send(2, 11, 0.5, true));
        m.strips[7].sidechain = Some(9);
        let order = m.processing_order().unwrap();
        let pos = |s: usize| order.iter().position(|&x| x == s).unwrap();
        assert!(pos(1) < pos(5) && pos(5) < pos(10));
        assert!(pos(2) < pos(11));
        assert!(pos(9) < pos(7));
        // 5 already feeds 10 via a send; 10 back into 1 would loop.
        assert!(!m.can_route(10, 1));
        assert!(!m.can_send(10, 5));
        assert!(m.can_route(10, 2));
        // Sends between inserts in any direction, as long as there is no loop.
        assert!(m.add_send(10, 3, 0.2, false));
        assert!(!m.add_send(3, 1, 0.2, false));
        m.strips[7].output = 9;
        assert!(m.processing_order().is_none());
        m.sanitize();
        assert!(m.processing_order().is_some());
    }

    #[test]
    fn sanitize_drops_bad_and_looping_sends() {
        let mut m = Mixer::with_inserts(4);
        m.strips[1].sends = vec![
            Send {
                to: 2,
                level: 3.0,
                pre: false,
            },
            Send {
                to: 2,
                level: 0.5,
                pre: false,
            },
            Send {
                to: 1,
                level: 0.5,
                pre: false,
            },
            Send {
                to: 99,
                level: 0.5,
                pre: false,
            },
        ];
        m.strips[2].sends = vec![Send {
            to: 1,
            level: 0.5,
            pre: true,
        }];
        m.sanitize();
        assert!(m.processing_order().is_some());
        assert!(m.strips[1].sends.len() + m.strips[2].sends.len() <= 1);
        assert!(m
            .strips
            .iter()
            .flat_map(|s| &s.sends)
            .all(|s| s.level <= 1.0));
        let mut big = Mixer::with_inserts(1000);
        assert_eq!(big.inserts(), MAX_INSERTS);
        assert_eq!(big.add_insert(), None);
        big.strips.truncate(1);
        big.sanitize();
        assert_eq!(big.inserts(), 1);
    }

    #[test]
    fn removing_an_insert_renumbers_routing() {
        let mut m = Mixer::with_inserts(5);
        m.strips[1].output = 4;
        m.add_send(2, 3, 0.4, false);
        m.add_send(2, 5, 0.6, true);
        m.strips[5].sidechain = Some(3);
        assert!(m.remove_insert(3));
        assert_eq!(m.inserts(), 4);
        assert_eq!(m.strips[1].output, 3);
        assert_eq!(
            m.strips[2].sends,
            vec![Send {
                to: 4,
                level: 0.6,
                pre: true
            }]
        );
        assert_eq!(m.strips[4].sidechain, None);
        assert_eq!(m.strips[3].name, "Insert 4");
        assert!(!m.remove_insert(MASTER));
    }

    #[test]
    fn pan_laws_meet_their_centre_levels() {
        let db = |g: f32| 20.0 * g.log10();
        for (law, want) in [
            (PanLaw::Zero, 0.0),
            (PanLaw::Minus3, -3.01),
            (PanLaw::Minus4_5, -4.52),
            (PanLaw::Minus6, -6.02),
        ] {
            let (l, r) = law.gains(0.0);
            assert!(
                (db(l) - want).abs() < 0.02 && (l - r).abs() < 1e-6,
                "{law:?}: {l}"
            );
            let (l, r) = law.gains(1.0);
            assert!(
                l.abs() < 1e-6 && (r - 1.0).abs() < 1e-6,
                "{law:?} hard right"
            );
            assert_eq!(PanLaw::from_key(law.key()), Some(law));
        }
    }

    #[test]
    fn solo_keeps_the_path_audible() {
        let mut m = Mixer::with_inserts(8);
        m.strips[1].output = 2;
        m.add_send(1, 7, 0.3, false);
        m.strips[1].solo = true;
        let a = m.audible();
        assert!(a[MASTER] && a[1] && a[2] && a[7]);
        assert!(!a[3] && !a[8]);
        m.strips[2].mute = true;
        assert!(!m.audible()[2]);
    }
}
