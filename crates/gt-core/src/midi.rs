//! MIDI learn: controller (CC) messages bound to parameters.

use crate::{ParamId, Project};

/// Bindings kept per project, at most this many (one per controller and channel is plenty).
pub const MAX_MIDI_BINDINGS: usize = 256;

/// One MIDI controller driving one parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MidiBinding {
    /// MIDI channel 0 to 15 the controller listens on.
    pub channel: u8,
    /// Controller number 0 to 119 (120 to 127 are channel-mode messages).
    pub cc: u8,
    /// The parameter it moves.
    pub param: ParamId,
}

/// Highest controller number that can be learned.
pub const MAX_LEARN_CC: u8 = 119;

impl Project {
    /// Binds a controller to `param`, replacing whatever that controller drove before and any
    /// older controller of `param`. Returns false for a controller that cannot be learned.
    pub fn learn_midi(&mut self, channel: u8, cc: u8, param: ParamId) -> bool {
        if channel > 15 || cc > MAX_LEARN_CC {
            return false;
        }
        self.midi_map
            .retain(|b| b.param != param && !(b.channel == channel && b.cc == cc));
        if self.midi_map.len() >= MAX_MIDI_BINDINGS {
            return false;
        }
        self.midi_map.push(MidiBinding { channel, cc, param });
        true
    }

    /// Removes the binding of `param`. True if there was one.
    pub fn forget_midi(&mut self, param: ParamId) -> bool {
        let n = self.midi_map.len();
        self.midi_map.retain(|b| b.param != param);
        self.midi_map.len() != n
    }

    /// The binding that drives `param`, if any.
    pub fn midi_binding(&self, param: ParamId) -> Option<MidiBinding> {
        self.midi_map.iter().copied().find(|b| b.param == param)
    }

    /// Applies a controller message: moves every bound parameter to `value` (0 to 127) over
    /// its full range, along its taper. Returns the parameters that changed.
    pub fn apply_cc(&mut self, channel: u8, cc: u8, value: u8) -> Vec<ParamId> {
        let t = f32::from(value.min(127)) / 127.0;
        let targets: Vec<ParamId> = self
            .midi_map
            .iter()
            .filter(|b| b.channel == channel && b.cc == cc)
            .map(|b| b.param)
            .collect();
        targets
            .into_iter()
            .filter(|p| {
                let v = p.info().from_normalized(t);
                p.get(self) != Some(v) && p.set(self, v)
            })
            .collect()
    }

    /// Drops bindings to parameters that no longer exist, duplicates and out-of-range values.
    pub(crate) fn sanitize_midi(&mut self) {
        let mut seen = std::collections::HashSet::new();
        let valid: Vec<MidiBinding> = std::mem::take(&mut self.midi_map)
            .into_iter()
            .filter(|b| b.channel <= 15 && b.cc <= MAX_LEARN_CC && b.param.is_valid(self))
            .filter(|b| seen.insert((b.channel, b.cc)))
            .collect();
        let mut params = std::collections::HashSet::new();
        self.midi_map = valid
            .into_iter()
            .filter(|b| params.insert(b.param))
            .take(MAX_MIDI_BINDINGS)
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChannelParam, MASTER_VOLUME};

    #[test]
    fn a_controller_moves_its_parameter_along_the_taper() {
        let mut p = Project::demo();
        let pan = ParamId::Channel {
            channel: p.channels[0].id,
            param: ChannelParam::Pan,
        };
        assert!(p.learn_midi(0, 10, pan));
        assert_eq!(p.apply_cc(0, 10, 0), vec![pan]);
        assert_eq!(pan.get(&p), Some(-1.0));
        assert_eq!(p.apply_cc(0, 10, 127), vec![pan]);
        assert_eq!(pan.get(&p), Some(1.0));
        // Unchanged value or another channel: nothing moves.
        assert!(p.apply_cc(0, 10, 127).is_empty());
        assert!(p.apply_cc(1, 10, 0).is_empty());
        // The master fader follows its dB taper: the middle of the controller is the
        // middle of the knob, not half the gain.
        assert!(p.learn_midi(0, 7, MASTER_VOLUME));
        p.apply_cc(0, 7, 64);
        let t = MASTER_VOLUME.normalized(&p);
        assert!((t - 64.0 / 127.0).abs() < 1e-3, "{t}");
    }

    #[test]
    fn learning_replaces_old_bindings_and_sanitize_drops_dead_ones() {
        let mut p = Project::demo();
        let a = ParamId::Channel {
            channel: p.channels[0].id,
            param: ChannelParam::Volume,
        };
        let b = ParamId::Channel {
            channel: p.channels[1].id,
            param: ChannelParam::Volume,
        };
        assert!(p.learn_midi(0, 1, a));
        // The same controller learned for b moves only b.
        assert!(p.learn_midi(0, 1, b));
        assert_eq!(p.midi_map.len(), 1);
        assert_eq!(p.midi_binding(b).map(|x| x.cc), Some(1));
        // A second controller for b replaces the first.
        assert!(p.learn_midi(3, 2, b));
        assert_eq!(p.midi_map.len(), 1);
        assert!(!p.learn_midi(0, 120, a));
        assert!(!p.learn_midi(16, 1, a));
        assert!(p.forget_midi(b));
        assert!(p.midi_map.is_empty());

        assert!(p.learn_midi(0, 1, a));
        let id = p.channels[0].id;
        p.remove_channel(id);
        p.sanitize();
        assert!(p.midi_map.is_empty());
    }
}
