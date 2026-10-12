//! Audio inputs and recorded audio (Prompt 13).
//!
//! An [`AudioInput`] names one or two channels of the input device, counted from 0. Playlist
//! tracks record from one; mixer inserts can take one as a live source. The document only
//! stores the channel numbers, so a project opened with another interface keeps its choices
//! and shows the ones the device does not have as unavailable.

/// Most input channels the program reads from a device.
pub const MAX_INPUT_CHANNELS: usize = 32;

/// One mono channel or a stereo pair of the input device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AudioInput {
    /// First device channel, from 0.
    pub first: u8,
    /// True for the pair `first`, `first + 1`.
    pub stereo: bool,
}

impl AudioInput {
    /// A mono input.
    pub fn mono(channel: u8) -> Self {
        Self {
            first: channel,
            stereo: false,
        }
    }

    /// A stereo pair starting at `first`.
    pub fn stereo(first: u8) -> Self {
        Self {
            first,
            stereo: true,
        }
    }

    /// Channels this input uses (1 or 2).
    pub fn channels(self) -> usize {
        if self.stereo {
            2
        } else {
            1
        }
    }

    /// True if a device with `device_channels` inputs has every channel this input uses.
    pub fn fits(self, device_channels: usize) -> bool {
        usize::from(self.first) + self.channels() <= device_channels.min(MAX_INPUT_CHANNELS)
    }

    /// Label as musicians count channels: "In 3", "In 1-2".
    pub fn label(self) -> String {
        let n = u32::from(self.first) + 1;
        if self.stereo {
            format!("In {n}-{}", n + 1)
        } else {
            format!("In {n}")
        }
    }

    /// Text key for files ("3", "1-2"), the inverse of [`AudioInput::from_key`].
    pub fn key(self) -> String {
        self.label().trim_start_matches("In ").to_owned()
    }

    /// Parses [`AudioInput::key`].
    pub fn from_key(key: &str) -> Option<Self> {
        let num = |s: &str| {
            s.trim()
                .parse::<u32>()
                .ok()
                .filter(|&n| (1..=MAX_INPUT_CHANNELS as u32).contains(&n))
        };
        match key.split_once('-') {
            Some((a, b)) => {
                let a = num(a)?;
                (num(b)? == a + 1).then(|| Self::stereo((a - 1) as u8))
            }
            None => num(key).map(|n| Self::mono((n - 1) as u8)),
        }
        .filter(|i| i.fits(MAX_INPUT_CHANNELS))
    }
}

/// When the inputs of armed tracks are heard through the mixer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MonitorMode {
    /// Never (the interface's own direct monitoring, or none).
    Off,
    /// Always while armed, through the track's mixer strip and its effects.
    Mixer,
    /// While armed and stopped, and while recording; not while playing back without
    /// recording, so a take can be listened to without the live input on top.
    #[default]
    Auto,
}

impl MonitorMode {
    /// Menu names, in the order of [`MonitorMode::ALL`].
    pub const NAMES: [&'static str; 3] = ["Off", "Through the mixer", "Auto"];
    /// Every mode.
    pub const ALL: [Self; 3] = [Self::Off, Self::Mixer, Self::Auto];

    /// Text key for settings files.
    pub fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Mixer => "mixer",
            Self::Auto => "auto",
        }
    }

    /// Parses [`MonitorMode::key`]; unknown text gives the default.
    pub fn from_key(key: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|m| m.key() == key)
            .unwrap_or_default()
    }

    /// The menu name.
    pub fn name(self) -> &'static str {
        Self::NAMES[Self::ALL.iter().position(|&m| m == self).unwrap_or(0)]
    }
}

/// A stretch of a recorded file where the input did not arrive in time (an xrun, a full disk
/// buffer). The file keeps its timing: missing audio is written as silence, and a skip that
/// realigned the input is a point (`len_s == 0`). Slips in consecutive quanta are merged into
/// one gap covering them all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gap {
    /// Seconds from the start of the file.
    pub at_s: f64,
    /// Seconds of silence written in place of the missing audio.
    pub len_s: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_have_stable_keys_and_labels() {
        for i in [
            AudioInput::mono(0),
            AudioInput::stereo(2),
            AudioInput::mono(31),
        ] {
            assert_eq!(AudioInput::from_key(&i.key()), Some(i));
        }
        assert_eq!(AudioInput::stereo(0).label(), "In 1-2");
        assert_eq!(AudioInput::mono(4).key(), "5");
        for bad in ["0", "33", "1-3", "32-33", "x", "", "-"] {
            assert_eq!(AudioInput::from_key(bad), None, "{bad}");
        }
        assert!(AudioInput::stereo(0).fits(2));
        assert!(!AudioInput::stereo(1).fits(2));
        for m in MonitorMode::ALL {
            assert_eq!(MonitorMode::from_key(m.key()), m);
        }
        assert_eq!(MonitorMode::from_key("?"), MonitorMode::Auto);
    }
}
