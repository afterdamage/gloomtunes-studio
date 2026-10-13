//! Audio Unit (AUv2) hosting on macOS (Prompt 18, D117), through Apple's own AudioToolbox API
//! as bound by `objc2-audio-toolbox` (Zlib OR Apache-2.0 OR MIT).
//!
//! Audio Units are not files: the system registers them, and [`list`] asks it for every
//! effect, instrument and music effect without loading any. Each gets a pseudo path
//! (`au:<type>:<subtype>:<manufacturer>`) so the catalog, the crash guard and quarantine treat
//! it like a plugin file. An instance is an `AudioUnit` with non-interleaved 32-bit stereo in
//! and out, rendered in blocks of up to `RENDER_QUANTUM` frames; parameters come from the
//! global parameter list, state is the unit's `ClassInfo` property list in binary form, and
//! factory presets are its `FactoryPresets`. Editors (Cocoa views), extra outputs and MIDI out
//! are not hosted for Audio Units yet: their parameters show as sliders.

#![allow(unsafe_code)]

use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_core::{PluginFormat, PluginKind, PluginParamInfo, MAX_PLUGIN_PARAMS};
use gt_engine::RENDER_QUANTUM;
use objc2_audio_toolbox::*;
use objc2_core_audio_types::*;
use objc2_core_foundation::{
    CFArray, CFData, CFPropertyListCreateData, CFPropertyListCreateWithData, CFPropertyListFormat,
    CFRetained, CFString, CFType,
};

use crate::backend::{Activated, Instance};
use crate::catalog::PluginInfo;
use crate::host::Signals;
use crate::presets::{Preset, Source};
use process::{AuProcessor, Param};

mod process;
#[cfg(test)]
mod tests;

/// What the pseudo path of an Audio Unit starts with.
pub(crate) const PATH_PREFIX: &str = "au:";
/// What a saved Audio Unit state starts with (then the `ClassInfo` property list).
const STATE_MAGIC: &[u8; 4] = b"GTAU";

/// A four-character code as text ("aufx").
fn fourcc(v: u32) -> String {
    v.to_be_bytes()
        .iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '?'
            }
        })
        .collect()
}

fn parse_fourcc(s: &str) -> Option<u32> {
    let b = s.as_bytes();
    (b.len() == 4).then(|| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// The component description in an id (`type:subtype:manufacturer`).
fn desc_of(id: &str) -> Option<AudioComponentDescription> {
    let mut parts = id.splitn(3, ':');
    Some(AudioComponentDescription {
        componentType: parse_fourcc(parts.next()?)?,
        componentSubType: parse_fourcc(parts.next()?)?,
        componentManufacturer: parse_fourcc(parts.next()?)?,
        componentFlags: 0,
        componentFlagsMask: 0,
    })
}

/// Takes ownership of a CFString the system handed over (+1) and reads it.
unsafe fn take_string(s: *const CFString) -> Option<String> {
    let s = NonNull::new(s.cast_mut())?;
    // SAFETY: the caller passes a string it owns.
    let s: CFRetained<CFString> = unsafe { CFRetained::from_raw(s) };
    Some(s.to_string())
}

/// Every Audio Unit effect, instrument and music effect the system knows.
pub(crate) fn list() -> Vec<PluginInfo> {
    let mut out = Vec::new();
    for kind in [
        kAudioUnitType_Effect,
        kAudioUnitType_MusicDevice,
        kAudioUnitType_MusicEffect,
    ] {
        let mut want = AudioComponentDescription {
            componentType: kind,
            componentSubType: 0,
            componentManufacturer: 0,
            componentFlags: 0,
            componentFlagsMask: 0,
        };
        let mut c: AudioComponent = std::ptr::null_mut();
        // SAFETY: plain queries of the system's component registry.
        unsafe {
            loop {
                c = AudioComponentFindNext(c, NonNull::from(&mut want));
                if c.is_null() || out.len() >= 4096 {
                    break;
                }
                let mut d: AudioComponentDescription = std::mem::zeroed();
                if AudioComponentGetDescription(c, NonNull::from(&mut d)) != 0 {
                    continue;
                }
                let mut name: *const CFString = std::ptr::null();
                AudioComponentCopyName(c, NonNull::from(&mut name));
                let full = take_string(name).unwrap_or_default();
                // Names are "Manufacturer: Unit".
                let (vendor, name) = match full.split_once(": ") {
                    Some((v, n)) => (v.trim().to_owned(), n.trim().to_owned()),
                    None => (fourcc(d.componentManufacturer), full.trim().to_owned()),
                };
                let mut version = 0u32;
                AudioComponentGetVersion(c, NonNull::from(&mut version));
                let id = format!(
                    "{}:{}:{}",
                    fourcc(d.componentType),
                    fourcc(d.componentSubType),
                    fourcc(d.componentManufacturer)
                );
                out.push(PluginInfo {
                    format: PluginFormat::Au,
                    path: PathBuf::from(format!("{PATH_PREFIX}{id}")),
                    id,
                    name,
                    vendor,
                    version: format!(
                        "{}.{}.{}",
                        version >> 16,
                        (version >> 8) & 0xFF,
                        version & 0xFF
                    ),
                    description: String::new(),
                    features: vec![if kind == kAudioUnitType_MusicDevice {
                        "instrument".to_owned()
                    } else {
                        "audio-effect".to_owned()
                    }],
                });
            }
        }
    }
    out
}

/// An Audio Unit instance on the main thread.
pub(crate) struct AuPlugin {
    info: PluginInfo,
    signals: Arc<Signals>,
    unit: AudioUnit,
    params: Vec<Param>,
    instrument: bool,
    offline: bool,
    initialized: bool,
    /// Held by the processor while the engine has it.
    token: Arc<()>,
}

impl AuPlugin {
    /// Creates an instance of `info`.
    pub(crate) fn create(info: &PluginInfo, signals: Arc<Signals>) -> Result<Self, String> {
        let name = &info.name;
        let mut d = desc_of(&info.id).ok_or_else(|| format!("{name}: bad Audio Unit id"))?;
        let mut unit: AudioComponentInstance = std::ptr::null_mut();
        // SAFETY: plain calls; the instance is disposed of in `drop`.
        unsafe {
            let c = AudioComponentFindNext(std::ptr::null_mut(), NonNull::from(&mut d));
            if c.is_null() {
                return Err(format!("{name} is not installed"));
            }
            if AudioComponentInstanceNew(c, NonNull::from(&mut unit)) != 0 || unit.is_null() {
                return Err(format!("cannot create {name}"));
            }
        }
        Ok(Self {
            instrument: info.kind() == PluginKind::Instrument,
            info: info.clone(),
            signals,
            unit,
            params: Vec::new(),
            offline: false,
            initialized: false,
            token: Arc::new(()),
        })
    }

    /// Reads a property into `T`.
    unsafe fn get<T>(&self, id: u32, scope: u32, element: u32, out: &mut T) -> bool {
        let mut size = std::mem::size_of::<T>() as u32;
        // SAFETY: `out` is a `T` of `size` bytes.
        unsafe {
            AudioUnitGetProperty(
                self.unit,
                id,
                scope,
                element,
                NonNull::from(out).cast(),
                NonNull::from(&mut size),
            ) == 0
        }
    }

    unsafe fn set<T>(&self, id: u32, scope: u32, element: u32, value: &T) -> bool {
        // SAFETY: `value` is a `T`.
        unsafe {
            AudioUnitSetProperty(
                self.unit,
                id,
                scope,
                element,
                (value as *const T).cast(),
                std::mem::size_of::<T>() as u32,
            ) == 0
        }
    }

    fn uninitialize(&mut self) {
        if self.initialized {
            // SAFETY: an initialized unit that no processor uses any more.
            unsafe { AudioUnitUninitialize(self.unit) };
            self.initialized = false;
        }
    }

    /// The factory presets: number and name.
    fn presets(&self) -> Vec<(i32, String)> {
        let mut array: *const CFArray = std::ptr::null();
        // SAFETY: the property is a CFArray of AUPreset pointers. Units differ on whether the
        // array is handed over retained, so it is not released (a small leak at worst).
        unsafe {
            if !self.get(
                kAudioUnitProperty_FactoryPresets,
                kAudioUnitScope_Global,
                0,
                &mut array,
            ) {
                return Vec::new();
            }
            let Some(array) = array.as_ref() else {
                return Vec::new();
            };
            (0..array.count().min(4096))
                .filter_map(|i| {
                    let p = (array.value_at_index(i) as *const AUPreset).as_ref()?;
                    let name = p.presetName.as_ref().map(|s| s.to_string())?;
                    Some((p.presetNumber, name))
                })
                .collect()
        }
    }
}

impl Drop for AuPlugin {
    fn drop(&mut self) {
        self.uninitialize();
        // SAFETY: the instance created in `create`; nothing uses it any more.
        unsafe { AudioComponentInstanceDispose(self.unit) };
    }
}

/// The text a unit gives its units of measure.
fn unit_suffix(unit: AudioUnitParameterUnit) -> &'static str {
    match unit {
        AudioUnitParameterUnit::Percent | AudioUnitParameterUnit::EqualPowerCrossfade => "%",
        AudioUnitParameterUnit::Seconds => "s",
        AudioUnitParameterUnit::Milliseconds => "ms",
        AudioUnitParameterUnit::Hertz => "Hz",
        AudioUnitParameterUnit::Cents | AudioUnitParameterUnit::AbsoluteCents => "ct",
        AudioUnitParameterUnit::RelativeSemiTones => "st",
        AudioUnitParameterUnit::Decibels => "dB",
        AudioUnitParameterUnit::Degrees => "°",
        AudioUnitParameterUnit::Octaves => "oct",
        AudioUnitParameterUnit::BPM => "BPM",
        AudioUnitParameterUnit::Beats => "beats",
        AudioUnitParameterUnit::SampleFrames => "frames",
        _ => "",
    }
}

impl Instance for AuPlugin {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn signals(&self) -> &Arc<Signals> {
        &self.signals
    }

    fn read_params(&mut self) -> (Vec<PluginParamInfo>, Vec<f32>) {
        let (global, unit) = (kAudioUnitScope_Global, self.unit);
        let mut ids: Vec<u32> = Vec::new();
        // SAFETY: plain property queries with buffers we own, sized as the unit says.
        unsafe {
            let mut size = 0u32;
            if AudioUnitGetPropertyInfo(
                unit,
                kAudioUnitProperty_ParameterList,
                global,
                0,
                &mut size,
                std::ptr::null_mut(),
            ) == 0
                && size >= 4
            {
                ids = vec![0; size as usize / 4];
                let mut size = (ids.len() * 4) as u32;
                if AudioUnitGetProperty(
                    unit,
                    kAudioUnitProperty_ParameterList,
                    global,
                    0,
                    NonNull::new_unchecked(ids.as_mut_ptr()).cast(),
                    NonNull::from(&mut size),
                ) != 0
                {
                    ids.clear();
                }
                ids.truncate(size as usize / 4);
            }
        }
        ids.truncate(MAX_PLUGIN_PARAMS);
        let mut infos = Vec::new();
        let mut values = Vec::new();
        self.params.clear();
        for id in ids {
            // SAFETY: as above.
            let p = unsafe {
                let mut p: AudioUnitParameterInfo = std::mem::zeroed();
                if !self.get(kAudioUnitProperty_ParameterInfo, global, id, &mut p) {
                    continue;
                }
                p
            };
            let flags = p.flags;
            let name = if flags.contains(AudioUnitParameterOptions::Flag_HasCFNameString) {
                // SAFETY: the unit set the string; released when it says so.
                unsafe {
                    let s = p.cfNameString.as_ref().map(|s| s.to_string());
                    if flags.contains(AudioUnitParameterOptions::Flag_CFNameRelease) {
                        take_string(p.cfNameString);
                    }
                    s
                }
            } else {
                None
            };
            let name = name.unwrap_or_else(|| {
                let bytes: Vec<u8> = p
                    .name
                    .iter()
                    .take_while(|&&c| c != 0)
                    .map(|&c| c as u8)
                    .collect();
                String::from_utf8_lossy(&bytes).into_owned()
            });
            let (min, max) = if p.minValue.is_finite() && p.maxValue.is_finite() {
                (p.minValue, p.maxValue.max(p.minValue))
            } else {
                (0.0, 1.0)
            };
            let stepped = matches!(
                p.unit,
                AudioUnitParameterUnit::Indexed | AudioUnitParameterUnit::Boolean
            );
            let param = Param {
                id,
                min,
                max,
                stepped,
            };
            let writable = flags.contains(AudioUnitParameterOptions::Flag_IsWritable);
            let readable = flags.contains(AudioUnitParameterOptions::Flag_IsReadable);
            infos.push(PluginParamInfo {
                id,
                name,
                default: param.to_norm(p.defaultValue),
                steps: if stepped {
                    (max - min).round().clamp(0.0, 1e6) as u32
                } else {
                    0
                },
                automatable: writable,
                hidden: !readable && !writable,
            });
            let mut v = p.defaultValue;
            // SAFETY: a plain query.
            unsafe {
                AudioUnitGetParameter(unit, id, global, 0, NonNull::from(&mut v));
            }
            values.push(param.to_norm(v));
            self.params.push(param);
        }
        (infos, values)
    }

    fn value_text(&mut self, index: usize, value: f32) -> Option<String> {
        let p = *self.params.get(index)?;
        let plain = p.to_plain(value);
        let global = kAudioUnitScope_Global;
        // SAFETY: plain queries; strings and arrays the unit hands over are released.
        unsafe {
            let mut v = plain;
            let mut q = AudioUnitParameterStringFromValue {
                inParamID: p.id,
                inValue: NonNull::from(&mut v),
                outString: std::ptr::null(),
            };
            if self.get(
                kAudioUnitProperty_ParameterStringFromValue,
                global,
                0,
                &mut q,
            ) {
                if let Some(s) = take_string(q.outString) {
                    return Some(s);
                }
            }
            if p.stepped {
                let mut array: *const CFArray = std::ptr::null();
                if self.get(
                    kAudioUnitProperty_ParameterValueStrings,
                    global,
                    p.id,
                    &mut array,
                ) {
                    if let Some(a) = NonNull::new(array.cast_mut()) {
                        let a: CFRetained<CFArray> = CFRetained::from_raw(a);
                        let i = (plain - p.min).round() as isize;
                        if (0..a.count()).contains(&i) {
                            let s = (a.value_at_index(i) as *const CFString).as_ref()?;
                            return Some(s.to_string());
                        }
                    }
                }
            }
            let mut info: AudioUnitParameterInfo = std::mem::zeroed();
            let unit = if self.get(kAudioUnitProperty_ParameterInfo, global, p.id, &mut info) {
                if info
                    .flags
                    .contains(AudioUnitParameterOptions::Flag_CFNameRelease)
                {
                    take_string(info.cfNameString);
                }
                info.unit
            } else {
                AudioUnitParameterUnit::Generic
            };
            let suffix = unit_suffix(unit);
            let digits = if (p.max - p.min).abs() >= 100.0 { 0 } else { 2 };
            Some(if suffix.is_empty() {
                format!("{plain:.digits$}")
            } else {
                format!("{plain:.digits$} {suffix}")
            })
        }
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        let mut list: *const CFType = std::ptr::null();
        // SAFETY: ClassInfo is a property list handed over retained (+1).
        unsafe {
            if !self.get(
                kAudioUnitProperty_ClassInfo,
                kAudioUnitScope_Global,
                0,
                &mut list,
            ) {
                return None;
            }
            let list: CFRetained<CFType> = CFRetained::from_raw(NonNull::new(list.cast_mut())?);
            let data = CFPropertyListCreateData(
                None,
                Some(&list),
                CFPropertyListFormat::BinaryFormat_v1_0,
                0,
                std::ptr::null_mut(),
            )?;
            let mut out = STATE_MAGIC.to_vec();
            out.extend_from_slice(&data.to_vec());
            Some(out)
        }
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), String> {
        let bad = || format!("{} rejected its saved state", self.info.name);
        let body = state.strip_prefix(STATE_MAGIC).ok_or_else(bad)?;
        let data = CFData::from_bytes(body);
        // SAFETY: a property list made from our bytes, passed by reference.
        unsafe {
            let list = CFPropertyListCreateWithData(
                None,
                Some(&data),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
            .ok_or_else(bad)?;
            let ptr: *const CFType = &*list;
            if !self.set(
                kAudioUnitProperty_ClassInfo,
                kAudioUnitScope_Global,
                0,
                &ptr,
            ) {
                return Err(bad());
            }
        }
        Ok(())
    }

    fn set_offline(&mut self, offline: bool) {
        self.offline = offline;
    }

    fn activate(&mut self, sample_rate: u32) -> Result<Activated, String> {
        let name = self.info.name.clone();
        self.uninitialize();
        let float = std::mem::size_of::<f32>() as u32;
        let format = AudioStreamBasicDescription {
            mSampleRate: f64::from(sample_rate),
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagsNativeFloatPacked | kAudioFormatFlagIsNonInterleaved,
            mBytesPerPacket: float,
            mFramesPerPacket: 1,
            mBytesPerFrame: float,
            mChannelsPerFrame: 2,
            mBitsPerChannel: 32,
            mReserved: 0,
        };
        let (global, input, output) = (
            kAudioUnitScope_Global,
            kAudioUnitScope_Input,
            kAudioUnitScope_Output,
        );
        let frames = RENDER_QUANTUM as u32;
        let offline = u32::from(self.offline);
        let mut processor = AuProcessor::new(
            self.unit,
            Arc::clone(&self.token),
            self.params.clone().into(),
            !self.instrument,
        );
        // SAFETY: plain calls on an uninitialized unit, as AudioToolbox requires; the render
        // callback's data lives in the processor's heap block, which outlives the unit's use
        // of it (the unit is uninitialized before the processor is created again).
        let latency = unsafe {
            if !self.set(kAudioUnitProperty_StreamFormat, output, 0, &format) {
                return Err(format!("{name} cannot play 32-bit stereo"));
            }
            if !self.instrument {
                if !self.set(kAudioUnitProperty_StreamFormat, input, 0, &format) {
                    return Err(format!("{name} cannot take 32-bit stereo"));
                }
                let callback = processor.callback();
                self.set(kAudioUnitProperty_SetRenderCallback, input, 0, &callback);
            }
            self.set(kAudioUnitProperty_MaximumFramesPerSlice, global, 0, &frames);
            self.set(kAudioUnitProperty_OfflineRender, global, 0, &offline);
            if AudioUnitInitialize(self.unit) != 0 {
                return Err(format!("cannot start {name}"));
            }
            self.initialized = true;
            let mut seconds = 0.0_f64;
            self.get(kAudioUnitProperty_Latency, global, 0, &mut seconds);
            (seconds.max(0.0) * f64::from(sample_rate))
                .round()
                .min(f64::from(1 << 20)) as usize
        };
        processor.set_latency(latency);
        let (_tx, rx) = rtrb::RingBuffer::new(1);
        Ok((Box::new(processor), rx))
    }

    fn try_deactivate(&mut self) -> bool {
        if Arc::strong_count(&self.token) > 1 {
            return false;
        }
        self.uninitialize();
        true
    }

    fn main_thread_work(&mut self, _now: Instant) {}

    fn next_frame(&self, _now: Instant) -> Option<Duration> {
        None
    }

    fn factory_presets(&mut self) -> Vec<Preset> {
        self.presets()
            .into_iter()
            .map(|(number, name)| Preset {
                name,
                user: false,
                source: Source::AuPreset(number),
            })
            .collect()
    }

    fn load_factory(&mut self, source: &Source) -> Result<(), String> {
        let &Source::AuPreset(number) = source else {
            return Err("not an Audio Unit preset".to_owned());
        };
        let mut array: *const CFArray = std::ptr::null();
        // SAFETY: as in `presets`; the preset record is copied while the array is alive.
        unsafe {
            if !self.get(
                kAudioUnitProperty_FactoryPresets,
                kAudioUnitScope_Global,
                0,
                &mut array,
            ) {
                return Err("no presets".to_owned());
            }
            let array = array.as_ref().ok_or("no presets")?;
            let preset = (0..array.count())
                .filter_map(|i| (array.value_at_index(i) as *const AUPreset).as_ref())
                .find(|p| p.presetNumber == number)
                .ok_or("no such preset")?;
            let copy = AUPreset {
                presetNumber: preset.presetNumber,
                presetName: preset.presetName,
            };
            if !self.set(
                kAudioUnitProperty_PresentPreset,
                kAudioUnitScope_Global,
                0,
                &copy,
            ) {
                return Err(format!("{} could not load the preset", self.info.name));
            }
        }
        Ok(())
    }

    fn has_editor(&self) -> bool {
        false
    }

    fn editor_open(&self) -> bool {
        false
    }

    fn open_editor(&mut self, _title: &str) -> Result<(), String> {
        Err("Audio Unit editors are not hosted yet; use the sliders".to_owned())
    }

    fn close_editor(&mut self) {}

    fn poll_editor(&mut self) {}
}
