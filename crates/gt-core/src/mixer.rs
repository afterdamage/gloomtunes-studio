//! The mixer: a master strip, 64 insert strips and 4 send buses.
//!
//! Strips are addressed by index: 0 is the master, 1 to 64 are inserts and 65 to 68 are the
//! send buses. Channels feed a strip (master or an insert). Every strip except the master has
//! an output (another strip); inserts also have a level for each send bus, and any strip can
//! take another strip's signal as a compressor sidechain key. Together these form the routing
//! graph, which must be free of cycles; [`Mixer::processing_order`] sorts it.

use crate::effects::EffectSlot;

/// Number of insert strips.
pub const INSERTS: usize = 64;
/// Number of send buses.
pub const SENDS: usize = 4;
/// Total strips: master, inserts, sends.
pub const STRIPS: usize = 1 + INSERTS + SENDS;
/// Effect slots per strip.
pub const FX_SLOTS: usize = 10;
/// Index of the master strip.
pub const MASTER: usize = 0;
/// Index of the first send bus.
pub const FIRST_SEND: usize = 1 + INSERTS;

/// What a strip index is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripKind {
    /// The master strip, which feeds the audio device.
    Master,
    /// Insert strip, numbered from 1.
    Insert(usize),
    /// Send bus, numbered from 1.
    Send(usize),
}

impl StripKind {
    /// Kind of strip `index`.
    pub fn of(index: usize) -> Self {
        match index {
            MASTER => Self::Master,
            i if i < FIRST_SEND => Self::Insert(i),
            i => Self::Send(i - FIRST_SEND + 1),
        }
    }

    /// Short label: "M", "3", "S2".
    pub fn short(self) -> String {
        match self {
            Self::Master => "M".to_owned(),
            Self::Insert(n) => n.to_string(),
            Self::Send(n) => format!("S{n}"),
        }
    }

    /// Default name: "Master", "Insert 3", "Send 2".
    pub fn default_name(self) -> String {
        match self {
            Self::Master => "Master".to_owned(),
            Self::Insert(n) => format!("Insert {n}"),
            Self::Send(n) => format!("Send {n}"),
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
    /// Post-fader level into each send bus, linear 0 to 1 (inserts only).
    pub sends: [f32; SENDS],
    /// Strip whose output is the key for this strip's compressors' sidechain input.
    pub sidechain: Option<usize>,
    /// Effects, processed top to bottom.
    pub slots: [Option<EffectSlot>; FX_SLOTS],
}

impl MixerStrip {
    /// Fader maximum: +6 dB.
    pub const MAX_VOLUME: f32 = 1.995_262_3;
    /// New strips sit at unity, so routing a channel through one changes nothing.
    pub const DEFAULT_VOLUME: f32 = 1.0;

    fn new(index: usize) -> Self {
        Self {
            name: StripKind::of(index).default_name(),
            volume: Self::DEFAULT_VOLUME,
            pan: 0.0,
            mute: false,
            solo: false,
            phase_invert: false,
            output: MASTER,
            sends: [0.0; SENDS],
            sidechain: None,
            slots: Default::default(),
        }
    }
}

/// All strips. Always exactly [`STRIPS`] long.
#[derive(Debug, Clone, PartialEq)]
pub struct Mixer {
    /// Strips by index.
    pub strips: Vec<MixerStrip>,
}

impl Default for Mixer {
    fn default() -> Self {
        Self::new()
    }
}

impl Mixer {
    /// Every strip at unity, routed to the master, no effects.
    pub fn new() -> Self {
        Self {
            strips: (0..STRIPS).map(MixerStrip::new).collect(),
        }
    }

    /// Routing edges `(from, to)`: outputs, non-zero sends and sidechain keys.
    fn edges(&self) -> Vec<(usize, usize)> {
        let mut e = Vec::new();
        for (i, s) in self.strips.iter().enumerate() {
            if i != MASTER {
                e.push((i, s.output));
            }
            for (k, &level) in s.sends.iter().enumerate() {
                if level > 0.0 && i != MASTER {
                    e.push((i, FIRST_SEND + k));
                }
            }
            if let Some(src) = s.sidechain {
                e.push((src, i));
            }
        }
        e
    }

    /// True if `to` can be reached from `from` along `edges`.
    fn reaches(edges: &[(usize, usize)], from: usize, to: usize) -> bool {
        let mut seen = [false; STRIPS];
        let mut stack = vec![from];
        while let Some(n) = stack.pop() {
            if n == to {
                return true;
            }
            if n >= STRIPS || std::mem::replace(&mut seen[n], true) {
                continue;
            }
            stack.extend(edges.iter().filter(|e| e.0 == n).map(|e| e.1));
        }
        false
    }

    /// Strips that `strip` may output to: inserts go to the master, another insert or a send;
    /// sends go to the master or another send (so a send can never feed back into an insert).
    /// Targets that would close a loop are excluded.
    pub fn can_route(&self, strip: usize, target: usize) -> bool {
        if strip == MASTER || strip >= STRIPS || target >= STRIPS || strip == target {
            return false;
        }
        if strip >= FIRST_SEND && (1..FIRST_SEND).contains(&target) {
            return false;
        }
        let edges: Vec<_> = self
            .edges()
            .into_iter()
            .filter(|e| e.0 != strip || e.1 != self.strips[strip].output)
            .collect();
        !Self::reaches(&edges, target, strip)
    }

    /// Whether `src` may be the sidechain key of `strip` without closing a loop.
    pub fn can_sidechain(&self, strip: usize, src: usize) -> bool {
        if src == MASTER || src >= STRIPS || strip >= STRIPS || src == strip {
            return false;
        }
        let edges: Vec<_> = self
            .edges()
            .into_iter()
            .filter(|&e| Some(e.0) != self.strips[strip].sidechain || e.1 != strip)
            .collect();
        !Self::reaches(&edges, strip, src)
    }

    /// A processing order in which every strip comes after all strips that feed it, with the
    /// master last. `None` if the routing has a cycle.
    pub fn processing_order(&self) -> Option<Vec<usize>> {
        let edges = self.edges();
        let mut indegree = [0_usize; STRIPS];
        for &(_, to) in &edges {
            indegree[to] += 1;
        }
        // Kahn's algorithm; lowest index first keeps the order stable and readable.
        let mut ready: Vec<usize> = (1..STRIPS).rev().filter(|&i| indegree[i] == 0).collect();
        let mut order = Vec::with_capacity(STRIPS);
        while let Some(n) = ready.pop() {
            order.push(n);
            for &(from, to) in &edges {
                if from == n {
                    indegree[to] -= 1;
                    if indegree[to] == 0 && to != MASTER {
                        ready.push(to);
                        ready.sort_unstable_by(|a, b| b.cmp(a));
                    }
                }
            }
        }
        if order.len() != STRIPS - 1 || indegree[MASTER] != 0 {
            return None;
        }
        order.push(MASTER);
        Some(order)
    }

    /// Which strips are heard, after mute and solo. When any strip is soloed, a strip is
    /// heard only if it is soloed or lies on a path into or out of a soloed strip (so a soloed
    /// insert still reaches the master through its route and keeps its reverb send). The
    /// master is never silenced by solo. A muted strip is always silent.
    pub fn audible(&self) -> [bool; STRIPS] {
        let mut out = [true; STRIPS];
        let soloed: Vec<usize> = (1..STRIPS).filter(|&i| self.strips[i].solo).collect();
        if !soloed.is_empty() {
            let edges = self.edges();
            for (i, o) in out.iter_mut().enumerate().skip(1) {
                *o = soloed
                    .iter()
                    .any(|&s| s == i || Self::reaches(&edges, i, s) || Self::reaches(&edges, s, i));
            }
        }
        for (o, s) in out.iter_mut().zip(&self.strips) {
            *o &= !s.mute;
        }
        out
    }

    /// Repairs a mixer read from a file or produced by a bug: strip count, ranges, and any
    /// routing that is invalid or cyclic (reset to the master).
    pub fn sanitize(&mut self) {
        self.strips.truncate(STRIPS);
        while self.strips.len() < STRIPS {
            self.strips.push(MixerStrip::new(self.strips.len()));
        }
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
            for v in &mut s.sends {
                *v = if v.is_finite() && (1..FIRST_SEND).contains(&i) {
                    v.clamp(0.0, 1.0)
                } else {
                    0.0
                };
            }
            if s.output >= STRIPS || s.output == i || i == MASTER {
                s.output = MASTER;
            }
            if s.sidechain
                .is_some_and(|x| x >= STRIPS || x == i || x == MASTER)
            {
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
        for i in 1..STRIPS {
            let target = self.strips[i].output;
            if target != MASTER && !self.can_route(i, target) {
                self.strips[i].output = MASTER;
            }
            if let Some(src) = self.strips[i].sidechain {
                if !self.can_sidechain(i, src) {
                    self.strips[i].sidechain = None;
                }
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
        assert_eq!(order.len(), STRIPS);
        assert_eq!(*order.last().unwrap(), MASTER);
        assert_eq!(StripKind::of(65), StripKind::Send(1));
        assert_eq!(StripKind::of(3).short(), "3");
    }

    #[test]
    fn order_respects_routing_and_rejects_cycles() {
        let mut m = Mixer::new();
        m.strips[1].output = 5;
        m.strips[5].output = FIRST_SEND;
        m.strips[2].sends[1] = 0.5;
        m.strips[7].sidechain = Some(9);
        let order = m.processing_order().unwrap();
        let pos = |s: usize| order.iter().position(|&x| x == s).unwrap();
        assert!(pos(1) < pos(5) && pos(5) < pos(FIRST_SEND));
        assert!(pos(2) < pos(FIRST_SEND + 1));
        assert!(pos(9) < pos(7));
        // 5 already feeds the send bus via 1 → 5; routing 5 back into 1 would loop.
        assert!(!m.can_route(5, 1));
        assert!(m.can_route(5, 2));
        // Sends never route into inserts.
        assert!(!m.can_route(FIRST_SEND, 3));
        assert!(m.can_route(FIRST_SEND, FIRST_SEND + 1));
        // 7 is keyed by 9, so 7 cannot feed 9's key path the other way round.
        m.strips[7].output = 9;
        assert!(m.processing_order().is_none());
        m.sanitize();
        assert!(m.processing_order().is_some());
        assert!(!m.can_sidechain(9, 7) || m.strips[7].output != 9);
    }

    #[test]
    fn solo_keeps_the_path_audible() {
        let mut m = Mixer::new();
        m.strips[1].output = 2;
        m.strips[1].sends[0] = 0.3;
        m.strips[1].solo = true;
        let a = m.audible();
        assert!(a[MASTER] && a[1] && a[2] && a[FIRST_SEND]);
        assert!(!a[3] && !a[FIRST_SEND + 1]);
        m.strips[2].mute = true;
        assert!(!m.audible()[2]);
    }
}
