//! The engine side of an Audio Unit: renders blocks, feeds an effect its input through the
//! render callback, and sends notes and parameter changes. Nothing here allocates.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;

use gt_engine::{PluginContext, PluginProcessor, RENDER_QUANTUM};
use objc2_audio_toolbox::*;
use objc2_core_audio_types::*;

/// How one parameter maps between normalized and the unit's plain values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Param {
    pub(crate) id: u32,
    pub(crate) min: f32,
    pub(crate) max: f32,
    pub(crate) stepped: bool,
}

impl Param {
    pub(crate) fn to_plain(self, norm: f32) -> f32 {
        let v = self.min + norm.clamp(0.0, 1.0) * (self.max - self.min);
        if self.stepped {
            v.round()
        } else {
            v
        }
    }

    pub(crate) fn to_norm(self, plain: f32) -> f32 {
        let span = self.max - self.min;
        if span.abs() < 1e-12 || !plain.is_finite() {
            0.0
        } else {
            ((plain - self.min) / span).clamp(0.0, 1.0)
        }
    }
}

/// An effect's input for the current block, read by the render callback.
struct Input {
    l: [f32; RENDER_QUANTUM],
    r: [f32; RENDER_QUANTUM],
}

/// A buffer list with room for two channels.
#[repr(C)]
struct Buffers {
    count: u32,
    buffers: [AudioBuffer; 2],
}

/// The engine-side Audio Unit.
pub(crate) struct AuProcessor {
    unit: AudioUnit,
    /// Held while the engine has the processor; the instance uninitializes only without it.
    _token: Arc<()>,
    params: Arc<[Param]>,
    effect: bool,
    latency: usize,
    input: Box<Input>,
    out_l: [f32; RENDER_QUANTUM],
    out_r: [f32; RENDER_QUANTUM],
    sample_time: f64,
    held: [bool; 128],
}

// SAFETY: the processor is used by one thread at a time: built on the UI thread, then only
// touched by the audio thread (or the export) until it is handed back. AUv2 units may be
// rendered from any one thread at a time.
unsafe impl Send for AuProcessor {}

/// Copies the effect's input into the buffers the unit asks for.
unsafe extern "C-unwind" fn render_input(
    refcon: NonNull<c_void>,
    _flags: NonNull<AudioUnitRenderActionFlags>,
    _time: NonNull<AudioTimeStamp>,
    _bus: u32,
    frames: u32,
    data: *mut AudioBufferList,
) -> i32 {
    // SAFETY: `refcon` is the processor's `Input`, alive while the unit is initialized; `data`
    // is the unit's buffer list with `mNumberBuffers` buffers.
    unsafe {
        let input = refcon.cast::<Input>().as_ref();
        let Some(list) = data.as_mut() else {
            return 0;
        };
        let n = (frames as usize).min(RENDER_QUANTUM);
        let first = std::ptr::addr_of_mut!(list.mBuffers).cast::<AudioBuffer>();
        for i in 0..list.mNumberBuffers as usize {
            let b = &mut *first.add(i);
            let src = if i == 0 { &input.l } else { &input.r };
            if b.mData.is_null() {
                b.mData = src.as_ptr() as *mut c_void;
            } else {
                let dst = std::slice::from_raw_parts_mut(b.mData.cast::<f32>(), n);
                dst.copy_from_slice(&src[..n]);
            }
            b.mDataByteSize = (n * 4) as u32;
        }
    }
    0
}

impl AuProcessor {
    pub(crate) fn new(unit: AudioUnit, token: Arc<()>, params: Arc<[Param]>, effect: bool) -> Self {
        Self {
            unit,
            _token: token,
            params,
            effect,
            latency: 0,
            input: Box::new(Input {
                l: [0.0; RENDER_QUANTUM],
                r: [0.0; RENDER_QUANTUM],
            }),
            out_l: [0.0; RENDER_QUANTUM],
            out_r: [0.0; RENDER_QUANTUM],
            sample_time: 0.0,
            held: [false; 128],
        }
    }

    /// The render callback that feeds the unit this processor's input.
    pub(crate) fn callback(&mut self) -> AURenderCallbackStruct {
        AURenderCallbackStruct {
            inputProc: Some(render_input),
            inputProcRefCon: (&mut *self.input as *mut Input).cast(),
        }
    }

    pub(crate) fn set_latency(&mut self, frames: usize) {
        self.latency = frames;
    }

    fn send(&mut self, offset: u32, msg: [u8; 3]) {
        let offset = offset.min(RENDER_QUANTUM as u32 - 1);
        // SAFETY: an initialized unit; MIDI events are queued for the next render.
        unsafe {
            MusicDeviceMIDIEvent(
                self.unit,
                u32::from(msg[0]),
                u32::from(msg[1]),
                u32::from(msg[2]),
                offset,
            );
        }
    }
}

impl PluginProcessor for AuProcessor {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _ctx: &PluginContext) -> bool {
        let n = l.len().min(r.len()).min(RENDER_QUANTUM);
        if n == 0 {
            return true;
        }
        if self.effect {
            self.input.l[..n].copy_from_slice(&l[..n]);
            self.input.r[..n].copy_from_slice(&r[..n]);
        }
        let bytes = (n * 4) as u32;
        let mut list = Buffers {
            count: 2,
            buffers: [
                AudioBuffer {
                    mNumberChannels: 1,
                    mDataByteSize: bytes,
                    mData: self.out_l.as_mut_ptr().cast(),
                },
                AudioBuffer {
                    mNumberChannels: 1,
                    mDataByteSize: bytes,
                    mData: self.out_r.as_mut_ptr().cast(),
                },
            ],
        };
        // SAFETY: AudioTimeStamp is plain data; all zero is valid.
        let mut time: AudioTimeStamp = unsafe { std::mem::zeroed() };
        time.mSampleTime = self.sample_time;
        time.mFlags = AudioTimeStampFlags::SampleTimeValid;
        let mut flags = AudioUnitRenderActionFlags(0);
        // SAFETY: the buffer list points at our two output arrays, `n` frames each.
        let status = unsafe {
            AudioUnitRender(
                self.unit,
                &mut flags,
                NonNull::from(&mut time),
                0,
                n as u32,
                NonNull::from(&mut list).cast(),
            )
        };
        self.sample_time += n as f64;
        if status != 0 {
            return false;
        }
        // The unit may hand back its own buffers.
        // SAFETY: the unit filled `n` frames at each pointer.
        unsafe {
            let a = std::slice::from_raw_parts(list.buffers[0].mData.cast::<f32>(), n);
            let b = std::slice::from_raw_parts(list.buffers[1].mData.cast::<f32>(), n);
            l[..n].copy_from_slice(a);
            r[..n].copy_from_slice(b);
        }
        true
    }

    fn note_on(&mut self, offset: u32, key: u8, velocity: f32) {
        let key = key & 0x7F;
        self.held[usize::from(key)] = true;
        let vel = (velocity.clamp(0.0, 1.0) * 127.0).round().max(1.0) as u8;
        self.send(offset, [0x90, key, vel]);
    }

    fn note_off(&mut self, offset: u32, key: u8) {
        let key = key & 0x7F;
        self.held[usize::from(key)] = false;
        self.send(offset, [0x80, key, 0]);
    }

    fn release_all(&mut self, offset: u32) {
        for key in 0..128u8 {
            if self.held[usize::from(key)] {
                self.note_off(offset, key);
            }
        }
    }

    fn midi(&mut self, offset: u32, msg: [u8; 3]) {
        self.send(offset, msg);
    }

    fn key_pressure(&mut self, offset: u32, key: u8, value: f32) {
        let v = (value.clamp(0.0, 1.0) * 127.0).round() as u8;
        self.send(offset, [0xA0, key & 0x7F, v]);
    }

    fn set_param(&mut self, index: u32, value: f32) {
        let Some(&p) = self.params.get(index as usize) else {
            return;
        };
        // SAFETY: a plain call on an initialized unit.
        unsafe {
            AudioUnitSetParameter(
                self.unit,
                p.id,
                kAudioUnitScope_Global,
                0,
                p.to_plain(value),
                0,
            );
        }
    }

    fn reset(&mut self) {
        self.held = [false; 128];
        // SAFETY: a plain call on an initialized unit.
        unsafe {
            AudioUnitReset(self.unit, kAudioUnitScope_Global, 0);
        }
    }

    fn latency(&self) -> usize {
        self.latency
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_map_between_normalized_and_plain() {
        let p = Param {
            id: 0,
            min: -20.0,
            max: 20.0,
            stepped: false,
        };
        assert_eq!(p.to_plain(0.75), 10.0);
        assert_eq!(p.to_norm(-20.0), 0.0);
        let s = Param {
            stepped: true,
            min: 0.0,
            max: 3.0,
            ..p
        };
        assert_eq!(s.to_plain(0.4), 1.0);
    }
}
