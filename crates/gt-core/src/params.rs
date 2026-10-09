//! Every automatable parameter in a project, addressed by a stable [`ParamId`].
//!
//! A `ParamId` names one control: a channel's volume, a Gloom Synth knob, a mixer fader, a send
//! level or an effect parameter. It knows the control's [`ParamInfo`] (range, taper, display
//! format), how to read and write the value in a [`Project`], and a text key such as
//! `channel/3/synth/filter.cutoff` that stays the same for as long as the thing it points at
//! exists, so project files (Step 9) and MIDI learn (Step 10) can store it.
//!
//! Automation and modulation work in normalized units (0..1 of the knob's travel); `info`
//! converts to plain values. Channels are addressed by [`ChannelId`], mixer strips and effect
//! slots by index (they are fixed positions on the mixer).

use crate::effects::EffectKind;
use crate::mixer::{MixerStrip, StripKind, FX_SLOTS, MASTER, SENDS, STRIPS};
use crate::param::ParamCurve::{Linear, Power};
use crate::param::ParamUnit::{Gain, Pan, Percent, Semitones, SignedPercent};
use crate::param::{ParamCurve, ParamInfo, ParamUnit};
use crate::plugin::{PluginOwner, PluginRef};
use crate::project::{Channel, ChannelId, Instrument, Project};
use crate::synth::{SynthParam, MOD_SLOTS};

/// Longest sampler attack and decay, in milliseconds.
pub const SAMPLER_MAX_AD_MS: f32 = 2000.0;
/// Longest sampler release, in milliseconds.
pub const SAMPLER_MAX_R_MS: f32 = 4000.0;

/// Channel parameters: the rack's volume and pan, and the sampler's settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelParam {
    /// Channel volume (any instrument).
    Volume,
    /// Channel pan (any instrument).
    Pan,
    /// Sampler transposition.
    Pitch,
    /// Sampler start point.
    Start,
    /// Sampler end point.
    End,
    /// Sampler envelope attack.
    Attack,
    /// Sampler envelope decay.
    Decay,
    /// Sampler envelope sustain.
    Sustain,
    /// Sampler envelope release.
    Release,
}

const fn info(
    key: &'static str,
    name: &'static str,
    (min, max, default): (f32, f32, f32),
    curve: ParamCurve,
    unit: ParamUnit,
) -> ParamInfo {
    ParamInfo {
        key,
        name,
        min,
        max,
        default,
        curve,
        unit,
        choices: &[],
    }
}

static CHANNEL_INFO: [ParamInfo; 9] = [
    info(
        "volume",
        "Volume",
        (0.0, Channel::MAX_VOLUME, Channel::DEFAULT_VOLUME),
        Linear,
        Gain,
    ),
    info("pan", "Pan", (-1.0, 1.0, 0.0), Linear, Pan),
    info("pitch", "Pitch", (-24.0, 24.0, 0.0), Linear, Semitones),
    info("start", "Start", (0.0, 1.0, 0.0), Linear, Percent),
    info("end", "End", (0.0, 1.0, 1.0), Linear, Percent),
    info(
        "attack",
        "Attack",
        (0.0, SAMPLER_MAX_AD_MS, 0.0),
        Power(2.0),
        ParamUnit::Ms,
    ),
    info(
        "decay",
        "Decay",
        (0.0, SAMPLER_MAX_AD_MS, 0.0),
        Power(2.0),
        ParamUnit::Ms,
    ),
    info("sustain", "Sustain", (0.0, 1.0, 1.0), Linear, Percent),
    info(
        "release",
        "Release",
        (0.0, SAMPLER_MAX_R_MS, 50.0),
        Power(2.0),
        ParamUnit::Ms,
    ),
];

impl ChannelParam {
    /// Every channel parameter, in a stable order.
    pub const ALL: [ChannelParam; 9] = [
        Self::Volume,
        Self::Pan,
        Self::Pitch,
        Self::Start,
        Self::End,
        Self::Attack,
        Self::Decay,
        Self::Sustain,
        Self::Release,
    ];

    /// Range, taper and format.
    pub fn info(self) -> &'static ParamInfo {
        &CHANNEL_INFO[self as usize]
    }

    /// True for the parameters only a sampler channel has.
    pub fn sampler_only(self) -> bool {
        !matches!(self, Self::Volume | Self::Pan)
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.info().key == key)
    }
}

/// Mixer strip parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StripParam {
    /// The fader.
    Volume,
    /// The balance knob.
    Pan,
    /// Level of one of the four sends (inserts only).
    Send(usize),
}

static STRIP_VOLUME: ParamInfo = info(
    "volume",
    "Volume",
    (0.0, MixerStrip::MAX_VOLUME, MixerStrip::DEFAULT_VOLUME),
    Power(3.0),
    Gain,
);
static STRIP_PAN: ParamInfo = info("pan", "Pan", (-1.0, 1.0, 0.0), Linear, Pan);
static STRIP_SEND: ParamInfo = info("send", "Send", (0.0, 1.0, 0.0), Linear, Gain);
static MOD_AMOUNT: ParamInfo = info("amount", "Amount", (-1.0, 1.0, 0.0), Linear, SignedPercent);
/// Plugin parameters are handled normalized (0..1 of the plugin's own range); the plugin host
/// converts, and the plugin formats the value for display.
static PLUGIN_PARAM: ParamInfo = info("value", "Value", (0.0, 1.0, 0.0), Linear, Percent);

/// Stable address of one automatable parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParamId {
    /// A channel's volume, pan or sampler setting.
    Channel {
        /// The channel.
        channel: ChannelId,
        /// Which setting.
        param: ChannelParam,
    },
    /// A Gloom Synth knob of a synth channel.
    Synth {
        /// The channel.
        channel: ChannelId,
        /// Which knob.
        param: SynthParam,
    },
    /// The amount of one row of a Gloom Synth's modulation matrix.
    SynthMod {
        /// The channel.
        channel: ChannelId,
        /// Matrix row, `0..MOD_SLOTS`.
        slot: usize,
    },
    /// A mixer strip's fader, balance or send.
    Strip {
        /// Strip index ([`MASTER`], an insert or a send bus).
        strip: usize,
        /// Which control.
        param: StripParam,
    },
    /// One parameter of the effect in a slot. Valid only while the slot holds that kind.
    Effect {
        /// Strip index.
        strip: usize,
        /// Slot index.
        slot: usize,
        /// The effect the parameter belongs to.
        kind: EffectKind,
        /// Index in [`EffectKind::params`].
        index: usize,
    },
    /// A parameter of a plugin (a channel's instrument or an effect slot). Valid only while
    /// the owner holds the plugin it was made for.
    Plugin {
        /// Where the plugin sits.
        owner: PluginOwner,
        /// [`PluginRef::id_hash`] of the plugin.
        plugin: u32,
        /// The plugin's id for the parameter.
        id: u32,
    },
}

impl ParamId {
    /// A strip's fader.
    pub fn strip_volume(strip: usize) -> Self {
        Self::Strip {
            strip,
            param: StripParam::Volume,
        }
    }

    /// A strip's balance.
    pub fn strip_pan(strip: usize) -> Self {
        Self::Strip {
            strip,
            param: StripParam::Pan,
        }
    }

    /// Range, taper and display format.
    pub fn info(&self) -> &'static ParamInfo {
        match *self {
            Self::Channel { param, .. } => param.info(),
            Self::Synth { param, .. } => param.info(),
            Self::SynthMod { .. } => &MOD_AMOUNT,
            Self::Strip { param, .. } => match param {
                StripParam::Volume => &STRIP_VOLUME,
                StripParam::Pan => &STRIP_PAN,
                StripParam::Send(_) => &STRIP_SEND,
            },
            Self::Effect { kind, index, .. } => kind.params().get(index).unwrap_or(&MOD_AMOUNT),
            Self::Plugin { .. } => &PLUGIN_PARAM,
        }
    }

    /// The parameter `id` of `plugin`, sitting at `owner`.
    pub fn plugin_param(owner: PluginOwner, plugin: &PluginRef, id: u32) -> Self {
        Self::Plugin {
            owner,
            plugin: plugin.id_hash(),
            id,
        }
    }

    /// The text key, e.g. `channel/3/volume`, `channel/3/synth/filter.cutoff`,
    /// `channel/3/mod/2`, `mixer/5/send/1` or `mixer/5/fx/2/eq/b1.freq`.
    pub fn key(&self) -> String {
        match *self {
            Self::Channel { channel, param } => {
                format!("channel/{}/{}", channel.0, param.info().key)
            }
            Self::Synth { channel, param } => {
                format!("channel/{}/synth/{}", channel.0, param.info().key)
            }
            Self::SynthMod { channel, slot } => format!("channel/{}/mod/{slot}", channel.0),
            Self::Strip { strip, param } => match param {
                StripParam::Volume => format!("mixer/{strip}/volume"),
                StripParam::Pan => format!("mixer/{strip}/pan"),
                StripParam::Send(k) => format!("mixer/{strip}/send/{k}"),
            },
            Self::Effect {
                strip,
                slot,
                kind,
                index,
            } => format!(
                "mixer/{strip}/fx/{slot}/{}/{}",
                kind.key(),
                kind.params().get(index).map_or("?", |p| p.key)
            ),
            Self::Plugin { owner, plugin, id } => match owner {
                PluginOwner::Channel(c) => format!("channel/{}/plugin/{plugin:08x}/{id}", c.0),
                PluginOwner::Effect { strip, slot } => {
                    format!("mixer/{strip}/fx/{slot}/plugin/{plugin:08x}/{id}")
                }
            },
        }
    }

    /// Parses a [`key`](Self::key). Checks the shape and ranges, not whether the channel or
    /// effect exists.
    pub fn parse(key: &str) -> Option<Self> {
        let parts: Vec<&str> = key.split('/').collect();
        let num = |s: &str| s.parse::<usize>().ok();
        match parts.as_slice() {
            ["channel", id, rest @ ..] => {
                let channel = ChannelId(id.parse().ok()?);
                match rest {
                    ["plugin", h, k] => Some(Self::Plugin {
                        owner: PluginOwner::Channel(channel),
                        plugin: u32::from_str_radix(h, 16).ok()?,
                        id: k.parse().ok()?,
                    }),
                    ["synth", k] => Some(Self::Synth {
                        channel,
                        param: SynthParam::from_key(k)?,
                    }),
                    ["mod", k] => Some(Self::SynthMod {
                        channel,
                        slot: num(k).filter(|&s| s < MOD_SLOTS)?,
                    }),
                    [k] => Some(Self::Channel {
                        channel,
                        param: ChannelParam::from_key(k)?,
                    }),
                    _ => None,
                }
            }
            ["mixer", s, rest @ ..] => {
                let strip = num(s).filter(|&s| s < STRIPS)?;
                let param = match rest {
                    ["volume"] => StripParam::Volume,
                    ["pan"] => StripParam::Pan,
                    ["send", k] => StripParam::Send(num(k).filter(|&k| k < SENDS)?),
                    ["fx", slot, "plugin", h, k] => {
                        return Some(Self::Plugin {
                            owner: PluginOwner::Effect {
                                strip,
                                slot: num(slot).filter(|&s| s < FX_SLOTS)?,
                            },
                            plugin: u32::from_str_radix(h, 16).ok()?,
                            id: k.parse().ok()?,
                        });
                    }
                    ["fx", slot, kind, k] => {
                        let kind = EffectKind::from_key(kind)?;
                        return Some(Self::Effect {
                            strip,
                            slot: num(slot).filter(|&s| s < FX_SLOTS)?,
                            kind,
                            index: kind.params().iter().position(|p| p.key == *k)?,
                        });
                    }
                    _ => return None,
                };
                Some(Self::Strip { strip, param })
            }
            _ => None,
        }
    }

    /// The channel this parameter belongs to, if any.
    pub fn channel(&self) -> Option<ChannelId> {
        match *self {
            Self::Channel { channel, .. }
            | Self::Synth { channel, .. }
            | Self::SynthMod { channel, .. }
            | Self::Plugin {
                owner: PluginOwner::Channel(channel),
                ..
            } => Some(channel),
            _ => None,
        }
    }

    /// The plugin this parameter belongs to, if it exists and is still the one it was made for.
    pub fn plugin_ref<'p>(&self, project: &'p Project) -> Option<&'p PluginRef> {
        let Self::Plugin { owner, plugin, .. } = *self else {
            return None;
        };
        project.plugin(owner).filter(|p| p.id_hash() == plugin)
    }

    /// The control's own label, e.g. "Volume", "Filter Cutoff", "Mod 3 amount", "B1 Freq".
    pub fn label(&self) -> String {
        match *self {
            Self::Channel { param, .. } => param.info().name.to_owned(),
            Self::Synth { param, .. } => synth_label(param),
            Self::SynthMod { slot, .. } => format!("Mod {} amount", slot + 1),
            Self::Strip { param, .. } => match param {
                StripParam::Volume => "Volume".to_owned(),
                StripParam::Pan => "Pan".to_owned(),
                StripParam::Send(k) => format!("Send {}", k + 1),
            },
            Self::Effect { kind, index, .. } => {
                let Some(p) = kind.params().get(index) else {
                    return "?".to_owned();
                };
                // EQ bands share names ("Freq"): prefix the band from the key ("b3.freq").
                match p.key.split_once('.') {
                    Some((band, _)) => format!("{} {}", band.to_uppercase(), p.name),
                    None => p.name.to_owned(),
                }
            }
            Self::Plugin { id, .. } => format!("Parameter {id}"),
        }
    }

    /// Display name with its owner, e.g. "Bass · Filter Cutoff" or "Delay · EQ · B1 Freq".
    pub fn name(&self, project: &Project) -> String {
        let strip = |i: usize| {
            project
                .mixer
                .strips
                .get(i)
                .map_or_else(|| StripKind::of(i).short(), |s| s.name.clone())
        };
        match *self {
            Self::Channel { channel, .. }
            | Self::Synth { channel, .. }
            | Self::SynthMod { channel, .. } => {
                let owner = project
                    .channel_index(channel)
                    .map_or("(deleted)", |i| project.channels[i].name.as_str());
                format!("{owner} · {}", self.label())
            }
            Self::Strip { strip: s, .. } => format!("{} · {}", strip(s), self.label()),
            Self::Effect { strip: s, kind, .. } => {
                format!("{} · {} · {}", strip(s), kind.name(), self.label())
            }
            Self::Plugin { owner, id, .. } => {
                let place = match owner {
                    PluginOwner::Channel(c) => project.channel_index(c).map_or_else(
                        || "(deleted)".to_owned(),
                        |i| project.channels[i].name.clone(),
                    ),
                    PluginOwner::Effect { strip: s, .. } => strip(s),
                };
                match self.plugin_ref(project) {
                    Some(p) => {
                        let param = p
                            .param_index(id)
                            .map_or_else(|| self.label(), |i| p.params[i].name.clone());
                        format!("{place} · {} · {param}", p.name)
                    }
                    None => format!("{place} · (plugin gone) · {}", self.label()),
                }
            }
        }
    }

    /// Current value in plain units, or `None` if the target does not exist (a deleted
    /// channel, a sampler setting of a synth channel, an empty or different effect slot).
    pub fn get(&self, project: &Project) -> Option<f32> {
        match *self {
            Self::Channel { channel, param } => {
                let ch = &project.channels[project.channel_index(channel)?];
                if !param.sampler_only() {
                    return Some(if param == ChannelParam::Volume {
                        ch.volume
                    } else {
                        ch.pan
                    });
                }
                let s = ch.sampler()?;
                Some(match param {
                    ChannelParam::Pitch => s.pitch,
                    ChannelParam::Start => s.start,
                    ChannelParam::End => s.end,
                    ChannelParam::Attack => s.adsr.attack_ms,
                    ChannelParam::Decay => s.adsr.decay_ms,
                    ChannelParam::Sustain => s.adsr.sustain,
                    ChannelParam::Release => s.adsr.release_ms,
                    ChannelParam::Volume | ChannelParam::Pan => unreachable!("handled above"),
                })
            }
            Self::Synth { channel, param } => {
                let ch = &project.channels[project.channel_index(channel)?];
                Some(ch.synth()?.get(param))
            }
            Self::SynthMod { channel, slot } => {
                let ch = &project.channels[project.channel_index(channel)?];
                Some(ch.synth()?.mods.get(slot)?.amount)
            }
            Self::Strip { strip, param } => {
                let s = project.mixer.strips.get(strip)?;
                match param {
                    StripParam::Volume => Some(s.volume),
                    StripParam::Pan => Some(s.pan),
                    StripParam::Send(k) if matches!(StripKind::of(strip), StripKind::Insert(_)) => {
                        s.sends.get(k).copied()
                    }
                    StripParam::Send(_) => None,
                }
            }
            Self::Effect {
                strip,
                slot,
                kind,
                index,
            } => {
                let slot = project.mixer.strips.get(strip)?.slots.get(slot)?.as_ref()?;
                if slot.kind != kind {
                    return None;
                }
                slot.params.get(index).copied()
            }
            Self::Plugin { id, .. } => self.plugin_ref(project)?.value(id),
        }
    }

    /// True if the target exists in `project`.
    pub fn is_valid(&self, project: &Project) -> bool {
        self.get(project).is_some()
    }

    /// Sets the value (plain units, clamped to the range). Returns false if the target does not
    /// exist.
    pub fn set(&self, project: &mut Project, value: f32) -> bool {
        let v = self.info().clamp(value);
        match *self {
            Self::Channel { channel, param } => {
                let Some(i) = project.channel_index(channel) else {
                    return false;
                };
                let ch = &mut project.channels[i];
                match param {
                    ChannelParam::Volume => ch.volume = v,
                    ChannelParam::Pan => ch.pan = v,
                    _ => {
                        let Some(s) = ch.sampler_mut() else {
                            return false;
                        };
                        match param {
                            ChannelParam::Pitch => s.pitch = v,
                            ChannelParam::Start => s.start = v.min(s.end),
                            ChannelParam::End => s.end = v.max(s.start),
                            ChannelParam::Attack => s.adsr.attack_ms = v,
                            ChannelParam::Decay => s.adsr.decay_ms = v,
                            ChannelParam::Sustain => s.adsr.sustain = v,
                            ChannelParam::Release => s.adsr.release_ms = v,
                            ChannelParam::Volume | ChannelParam::Pan => unreachable!(),
                        }
                    }
                }
                true
            }
            Self::Synth { channel, param } => {
                let Some(i) = project.channel_index(channel) else {
                    return false;
                };
                match project.channels[i].synth_mut() {
                    Some(p) => {
                        p.set(param, v);
                        true
                    }
                    None => false,
                }
            }
            Self::SynthMod { channel, slot } => {
                let Some(i) = project.channel_index(channel) else {
                    return false;
                };
                match project.channels[i]
                    .synth_mut()
                    .and_then(|p| p.mods.get_mut(slot))
                {
                    Some(m) => {
                        m.amount = v;
                        true
                    }
                    None => false,
                }
            }
            Self::Strip { strip, param } => {
                if !self.is_valid(project) {
                    return false;
                }
                let s = &mut project.mixer.strips[strip];
                match param {
                    StripParam::Volume => s.volume = v,
                    StripParam::Pan => s.pan = v,
                    StripParam::Send(k) => s.sends[k] = v,
                }
                true
            }
            Self::Effect {
                strip, slot, index, ..
            } => {
                if !self.is_valid(project) {
                    return false;
                }
                let slot = project.mixer.strips[strip].slots[slot]
                    .as_mut()
                    .expect("checked by is_valid");
                slot.params[index] = v;
                true
            }
            Self::Plugin { owner, plugin, id } => project
                .plugin_mut(owner)
                .filter(|p| p.id_hash() == plugin)
                .is_some_and(|p| p.set_value(id, v)),
        }
    }

    /// Current value as a knob position (0..1); the default's position if the target is gone.
    pub fn normalized(&self, project: &Project) -> f32 {
        let i = self.info();
        i.to_normalized(self.get(project).unwrap_or(i.default))
    }

    /// Text for a normalized value, e.g. "-3.2 dB", "L 40 %", "1.20 kHz".
    pub fn format_normalized(&self, t: f32) -> String {
        let i = self.info();
        i.format(i.from_normalized(t))
    }

    /// Every parameter that exists in `project`: channels in rack order, then the mixer strips
    /// in index order with their effects.
    pub fn all(project: &Project) -> Vec<ParamId> {
        let mut out = Vec::new();
        for ch in &project.channels {
            out.extend(Self::of_channel(ch));
        }
        for strip in 0..STRIPS {
            out.extend(Self::of_strip(project, strip));
        }
        out
    }

    /// The parameters of one channel.
    pub fn of_channel(ch: &Channel) -> Vec<ParamId> {
        let channel = ch.id;
        let mut out: Vec<ParamId> = ChannelParam::ALL
            .into_iter()
            .filter(|p| !p.sampler_only() || matches!(ch.instrument, Instrument::Sampler(_)))
            .map(|param| Self::Channel { channel, param })
            .collect();
        if matches!(ch.instrument, Instrument::Synth(_)) {
            out.extend(
                SynthParam::ALL
                    .iter()
                    .map(|&param| Self::Synth { channel, param }),
            );
            out.extend((0..MOD_SLOTS).map(|slot| Self::SynthMod { channel, slot }));
        }
        if let Some(p) = ch.plugin() {
            out.extend(Self::of_plugin(PluginOwner::Channel(channel), p));
        }
        out
    }

    /// The automatable parameters of a plugin that the plugin does not hide.
    pub fn of_plugin(owner: PluginOwner, plugin: &PluginRef) -> impl Iterator<Item = ParamId> + '_ {
        plugin
            .params
            .iter()
            .filter(|p| p.automatable && !p.hidden)
            .map(move |p| Self::plugin_param(owner, plugin, p.id))
    }

    /// The parameters of one mixer strip, its sends and the effects in its slots.
    pub fn of_strip(project: &Project, strip: usize) -> Vec<ParamId> {
        let mut out = vec![Self::strip_volume(strip), Self::strip_pan(strip)];
        if matches!(StripKind::of(strip), StripKind::Insert(_)) {
            out.extend((0..SENDS).map(|k| Self::Strip {
                strip,
                param: StripParam::Send(k),
            }));
        }
        if let Some(s) = project.mixer.strips.get(strip) {
            for (slot, fx) in s.slots.iter().enumerate() {
                if let Some(p) = fx.as_ref().and_then(|f| f.plugin.as_deref()) {
                    out.extend(Self::of_plugin(PluginOwner::Effect { strip, slot }, p));
                }
                if let Some(fx) = fx {
                    out.extend((0..fx.kind.params().len()).map(|index| Self::Effect {
                        strip,
                        slot,
                        kind: fx.kind,
                        index,
                    }));
                }
            }
        }
        out
    }
}

/// "Filter Cutoff", "Osc 1 Wave", "LFO 2 Rate" from a synth key such as "filter.cutoff".
fn synth_label(p: SynthParam) -> String {
    let info = p.info();
    let section = match info.key.split_once('.').map(|(s, _)| s) {
        Some("osc1") => "Osc 1",
        Some("osc2") => "Osc 2",
        Some("sub") | Some("noise") => "",
        Some("unison") => "Unison",
        Some("filter") => "Filter",
        Some("amp") => "Amp",
        Some("mod") => "Mod env",
        Some("lfo1") => "LFO 1",
        Some("lfo2") => "LFO 2",
        _ => "",
    };
    if section.is_empty() {
        info.name.to_owned()
    } else {
        format!("{section} {}", info.name)
    }
}

/// The master strip's fader, the usual first automation target.
pub const MASTER_VOLUME: ParamId = ParamId::Strip {
    strip: MASTER,
    param: StripParam::Volume,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::EffectSlot;

    #[test]
    fn every_parameter_round_trips_its_key_and_value() {
        let mut p = Project::demo();
        p.mixer.strips[3].slots[4] = Some(EffectSlot::new(EffectKind::Eq));
        p.mixer.strips[3].slots[5] = Some(EffectSlot::plugin(test_plugin("fx", 2)));
        p.add_plugin_channel("Lead", test_plugin("synth", 3));
        let all = ParamId::all(&p);
        assert_eq!(
            all.iter()
                .filter(|id| matches!(id, ParamId::Plugin { .. }))
                .count(),
            5,
            "hidden and non-automatable parameters are left out"
        );
        assert!(all.len() > 200, "{}", all.len());
        let mut keys = std::collections::HashSet::new();
        for id in &all {
            let key = id.key();
            assert!(keys.insert(key.clone()), "duplicate key {key}");
            assert_eq!(ParamId::parse(&key), Some(*id), "{key}");
            assert!(id.is_valid(&p), "{key}");
            // Setting a value inside the range reads back.
            let info = id.info();
            let v = info.from_normalized(0.3);
            assert!(id.set(&mut p, v), "{key}");
            assert!((id.get(&p).unwrap() - v).abs() < 1e-4, "{key}");
            assert!(!id.name(&p).is_empty());
        }
    }

    fn test_plugin(id: &str, n: u32) -> PluginRef {
        use crate::plugin::{PluginKind, PluginParamInfo};
        let mut p = PluginRef::new(id, id, "Test", PluginKind::Effect, Default::default());
        let mut params: Vec<PluginParamInfo> = (0..n)
            .map(|i| PluginParamInfo {
                id: 100 + i,
                name: format!("Knob {i}"),
                default: 0.5,
                steps: 0,
                automatable: true,
                hidden: false,
            })
            .collect();
        params.push(PluginParamInfo {
            id: 999,
            name: "Hidden".into(),
            default: 0.0,
            steps: 0,
            automatable: true,
            hidden: true,
        });
        p.values = vec![0.5; params.len()];
        p.params = std::sync::Arc::from(params);
        p
    }

    #[test]
    fn plugin_parameters_follow_their_plugin() {
        let mut p = Project::demo();
        let ch = p
            .add_plugin_channel("Lead", test_plugin("a.synth", 2))
            .unwrap();
        let owner = PluginOwner::Channel(ch);
        let id = ParamId::plugin_param(owner, p.plugin(owner).unwrap(), 101);
        assert!(id.key().starts_with(&format!("channel/{}/plugin/", ch.0)));
        assert!(id.set(&mut p, 0.75));
        assert_eq!(id.get(&p), Some(0.75));
        assert_eq!(id.name(&p), "Lead · a.synth · Knob 1");
        assert_eq!(id.channel(), Some(ch));
        // Another plugin in the same place does not inherit the address.
        let i = p.channel_index(ch).unwrap();
        p.channels[i].instrument = Instrument::Plugin(Box::new(test_plugin("b.synth", 2)));
        assert!(!id.is_valid(&p));
        assert!(id.name(&p).contains("plugin gone"));
        let fx = ParamId::Plugin {
            owner: PluginOwner::Effect { strip: 2, slot: 9 },
            plugin: 0xdead_beef,
            id: 7,
        };
        assert_eq!(fx.key(), "mixer/2/fx/9/plugin/deadbeef/7");
        assert_eq!(ParamId::parse(&fx.key()), Some(fx));
        assert_eq!(ParamId::parse("mixer/2/fx/9/plugin/xyz/7"), None);
    }

    #[test]
    fn invalid_targets_are_rejected() {
        let mut p = Project::demo();
        let synth = p.channels.iter().find(|c| c.synth().is_some()).unwrap().id;
        let pitch = ParamId::Channel {
            channel: synth,
            param: ChannelParam::Pitch,
        };
        assert!(!pitch.is_valid(&p), "a synth has no sampler pitch");
        assert!(!pitch.set(&mut p, 3.0));
        let send_of_master = ParamId::Strip {
            strip: MASTER,
            param: StripParam::Send(0),
        };
        assert!(send_of_master.get(&p).is_none());
        let gone = ParamId::Channel {
            channel: ChannelId(9999),
            param: ChannelParam::Volume,
        };
        assert!(!gone.is_valid(&p));
        assert!(gone.name(&p).contains("deleted"));
        for bad in [
            "",
            "channel/x/volume",
            "channel/1/nope",
            "mixer/999/volume",
            "mixer/1/send/9",
            "mixer/1/fx/2/eq/zzz",
            "mixer/1/fx/99/eq/b1.freq",
        ] {
            assert_eq!(ParamId::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn labels_and_formats() {
        let p = Project::demo();
        assert_eq!(MASTER_VOLUME.name(&p), "Master · Volume");
        assert_eq!(
            MASTER_VOLUME.format_normalized(MASTER_VOLUME.info().to_normalized(1.0)),
            "+0.0 dB"
        );
        assert_eq!(ParamId::strip_pan(1).format_normalized(0.5), "C");
        assert_eq!(ParamId::strip_pan(1).format_normalized(0.0), "L 100 %");
        let eq = ParamId::Effect {
            strip: 1,
            slot: 0,
            kind: EffectKind::Eq,
            index: 5,
        };
        assert_eq!(eq.label(), "B2 Freq");
        let cutoff = ParamId::Synth {
            channel: ChannelId(1),
            param: SynthParam::Cutoff,
        };
        assert_eq!(cutoff.label(), "Filter Cutoff");
        assert_eq!(cutoff.key(), "channel/1/synth/filter.cutoff");
    }
}
