//! Audio device layer: the only code that talks to cpal.
//!
//! Lives on the UI thread. It enumerates devices, keeps one output stream open around an
//! `AudioProcessor`, converts samples at the edge, and turns device errors into status text.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig, SupportedBufferSize};
use gt_engine::{AudioProcessor, EngineCommand, EngineConfig, EngineHandle};

/// Frames of f32 scratch for devices that want integer samples. Larger callbacks are rendered
/// in several chunks, so this only bounds the chunk size, not the device buffer size.
const SCRATCH_FRAMES: usize = 4096;
/// If the fade-out has not been confirmed by the audio thread after this long (e.g. the device
/// stopped calling back), the stream is reopened anyway.
const STOP_TIMEOUT: Duration = Duration::from_millis(300);

struct Running {
    stream: cpal::Stream,
    engine: EngineHandle,
    channels: u16,
    sample_rate: u32,
    device_name: String,
}

/// Device layer state. The output stream stays open while the app runs, so the transport can
/// play at any time; it is faded out and reopened when the device or buffer size changes.
pub struct AudioIo {
    host: cpal::Host,
    /// `None` at index 0 means "follow the system default device".
    devices: Vec<Option<cpal::Device>>,
    names: Vec<String>,
    selected: usize,
    buffer_size: u32,
    running: Option<Running>,
    /// Set while the output fades out before the stream is reopened.
    reopening_since: Option<Instant>,
    /// True once after a new engine was created, so the app can send it the current settings.
    fresh_engine: bool,
    /// Engine events since the app last took them.
    events: Vec<gt_engine::EngineEvent>,
    stream_error: Arc<Mutex<Option<String>>>,
    status: String,
}

impl AudioIo {
    pub fn new() -> Self {
        let host = pick_host();
        log::info!("audio host: {}", host.id().name());
        let mut io = Self {
            host,
            devices: Vec::new(),
            names: Vec::new(),
            selected: 0,
            buffer_size: 256,
            running: None,
            reopening_since: None,
            fresh_engine: false,
            events: Vec::new(),
            stream_error: Arc::new(Mutex::new(None)),
            status: String::new(),
        };
        io.rescan();
        io.open();
        io
    }

    /// Re-enumerates output devices, keeping the selection by name when possible.
    pub fn rescan(&mut self) {
        let previous = self.names.get(self.selected).cloned();
        self.devices = vec![None];
        self.names = vec!["System default".to_owned()];
        match self.host.output_devices() {
            Ok(devices) => {
                for d in devices {
                    let name = device_name(&d);
                    self.devices.push(Some(d));
                    self.names.push(name);
                }
            }
            Err(e) => self.status = format!("Could not list devices: {e}"),
        }
        self.selected = previous
            .and_then(|p| self.names.iter().position(|n| *n == p))
            .unwrap_or(0);
    }

    pub fn select_device(&mut self, index: usize) {
        if index < self.devices.len() {
            self.selected = index;
            self.reopen();
        }
    }

    pub fn select_buffer_size(&mut self, frames: u32) {
        self.buffer_size = frames;
        self.reopen();
    }

    /// Closes the stream (after a short fade) and opens it again with the current settings.
    pub fn reopen(&mut self) {
        match self.running.as_mut() {
            Some(r) if self.reopening_since.is_none() => {
                let _ = r.engine.send(EngineCommand::FadeOut);
                self.reopening_since = Some(Instant::now());
            }
            Some(_) => {} // already fading
            None => self.open(),
        }
    }

    /// The engine of the open stream, if any.
    pub fn engine_mut(&mut self) -> Option<&mut EngineHandle> {
        self.running.as_mut().map(|r| &mut r.engine)
    }

    /// The engine of the open stream, if any.
    pub fn engine(&self) -> Option<&EngineHandle> {
        self.running.as_ref().map(|r| &r.engine)
    }

    /// True exactly once after a new engine has been created.
    pub fn take_fresh_engine(&mut self) -> bool {
        std::mem::take(&mut self.fresh_engine)
    }

    /// Engine events (transport changes, live notes) since the last call.
    pub fn take_events(&mut self) -> Vec<gt_engine::EngineEvent> {
        std::mem::take(&mut self.events)
    }

    /// Output buffer latency in milliseconds (the last callback's size), if a stream runs.
    pub fn buffer_ms(&self) -> Option<f32> {
        let e = self.engine()?;
        let frames = e.telemetry().last_block_frames.load(Ordering::Relaxed);
        Some(frames as f32 * 1000.0 / e.config().sample_rate.max(1) as f32)
    }

    /// Returns and resets the raw peak the audio thread has seen since the last call.
    pub fn take_peak(&self) -> f32 {
        self.engine()
            .map_or(0.0, |e| e.telemetry().peak.swap(0.0, Ordering::Relaxed))
    }

    /// Housekeeping, once per UI frame: frees engine garbage, surfaces stream errors and
    /// finishes reopening.
    pub fn poll(&mut self) {
        if let Some(msg) = self.stream_error.lock().ok().and_then(|mut e| e.take()) {
            log::warn!("stream error: {msg}");
            self.status = msg;
        }
        let Some(r) = self.running.as_mut() else {
            return;
        };
        r.engine.collect_garbage();
        let events = &mut self.events;
        r.engine.poll_events(|e| events.push(e));
        if r.engine.telemetry().faulted.load(Ordering::Relaxed) {
            self.status = "Audio engine fault: output muted. Use Restart audio.".to_owned();
            self.running = None;
            self.reopening_since = None;
            return;
        }
        if let Some(since) = self.reopening_since {
            let silent = r.engine.telemetry().silent.load(Ordering::Relaxed);
            if silent || since.elapsed() > STOP_TIMEOUT {
                self.running = None; // dropping the stream closes the device
                self.reopening_since = None;
                self.open();
            }
        }
    }

    pub fn panel_model(&self, test_tone: bool) -> gt_ui::views::AudioPanelModel {
        let (sample_rate, channels, callback_frames) = match &self.running {
            Some(r) => {
                let frames = r
                    .engine
                    .telemetry()
                    .last_block_frames
                    .load(Ordering::Relaxed);
                (
                    Some(r.sample_rate),
                    Some(r.channels),
                    (frames > 0).then_some(frames),
                )
            }
            None => (None, None, None),
        };
        gt_ui::views::AudioPanelModel {
            host_name: self.host.id().name().to_owned(),
            devices: self.names.clone(),
            selected_device: self.selected,
            buffer_size: self.buffer_size,
            sample_rate,
            channels,
            callback_frames,
            stream_open: self.running.is_some(),
            test_tone,
            level_db: f32::NEG_INFINITY,
            status: self.status.clone(),
        }
    }

    fn device(&self) -> Option<cpal::Device> {
        match self.devices.get(self.selected) {
            Some(Some(d)) => Some(d.clone()),
            _ => self.host.default_output_device(),
        }
    }

    fn open(&mut self) {
        match self.open_stream() {
            Ok(running) => {
                self.status = format!("Audio running on {}", running.device_name);
                self.running = Some(running);
                self.fresh_engine = true;
            }
            Err(e) => {
                log::error!("{e}");
                self.status = e;
            }
        }
    }

    fn open_stream(&mut self) -> Result<Running, String> {
        let device = self.device().ok_or("No output device available")?;
        let supported = device
            .default_output_config()
            .map_err(|e| format!("Device has no usable output config: {e}"))?;
        let sample_format = supported.sample_format();
        let mut config: StreamConfig = supported.config();

        // Ask for the selected buffer size if the device reports it can do it. cpal treats this
        // as a request: the actual callback size is shown in the UI from telemetry.
        let note = match supported.buffer_size() {
            SupportedBufferSize::Range { min, max }
                if (*min..=*max).contains(&self.buffer_size) =>
            {
                config.buffer_size = cpal::BufferSize::Fixed(self.buffer_size);
                String::new()
            }
            SupportedBufferSize::Range { min, max } => {
                format!(" (device supports {min}–{max} frames; using its default)")
            }
            _ => {
                config.buffer_size = cpal::BufferSize::Fixed(self.buffer_size);
                String::new()
            }
        };

        let (engine, processor) = gt_engine::create(EngineConfig {
            sample_rate: config.sample_rate,
            out_channels: usize::from(config.channels),
        });
        let error_slot = Arc::clone(&self.stream_error);
        let stream = match sample_format {
            SampleFormat::F32 => build_f32(&device, &config, processor, error_slot),
            SampleFormat::F64 => build_converted::<f64>(&device, &config, processor, error_slot),
            SampleFormat::I16 => build_converted::<i16>(&device, &config, processor, error_slot),
            SampleFormat::I32 => build_converted::<i32>(&device, &config, processor, error_slot),
            SampleFormat::U16 => build_converted::<u16>(&device, &config, processor, error_slot),
            SampleFormat::U8 => build_converted::<u8>(&device, &config, processor, error_slot),
            SampleFormat::I8 => build_converted::<i8>(&device, &config, processor, error_slot),
            other => return Err(format!("Unsupported device sample format {other}")),
        }
        .map_err(|e| format!("Could not open the output stream: {e}"))?;
        stream
            .play()
            .map_err(|e| format!("Could not start the output stream: {e}"))?;

        log::info!(
            "stream open: {} Hz, {} ch, {sample_format}, buffer {:?}",
            config.sample_rate,
            config.channels,
            config.buffer_size
        );
        Ok(Running {
            stream,
            engine,
            channels: config.channels,
            sample_rate: config.sample_rate,
            device_name: format!("{}{note}", device_name(&device)),
        })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // Pausing first makes the backend stop calling back before the stream is torn down.
        let _ = self.stream.pause();
    }
}

/// Uses the host named by `GT_AUDIO_HOST` (e.g. `jack`, `pipewire`, `asio`) when it was compiled
/// in, otherwise the platform default (WASAPI on Windows, ALSA on Linux).
fn pick_host() -> cpal::Host {
    if let Ok(wanted) = std::env::var("GT_AUDIO_HOST") {
        let found = cpal::available_hosts()
            .into_iter()
            .find(|id| id.name().eq_ignore_ascii_case(&wanted));
        match found.map(cpal::host_from_id) {
            Some(Ok(host)) => return host,
            Some(Err(e)) => log::warn!("audio host {wanted} unavailable: {e}"),
            None => log::warn!("audio host {wanted} not compiled in; using the default"),
        }
    }
    cpal::default_host()
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| "Unknown device".to_owned())
}

fn error_callback(slot: Arc<Mutex<Option<String>>>) -> impl FnMut(cpal::Error) + Send + 'static {
    // Runs on a backend thread, never on the audio callback, so a lock is fine here.
    move |err| {
        if let Ok(mut s) = slot.lock() {
            *s = Some(format!("Audio device: {err}"));
        }
    }
}

/// f32 devices: the engine renders straight into the device buffer.
fn build_f32(
    device: &cpal::Device,
    config: &StreamConfig,
    mut processor: AudioProcessor,
    error_slot: Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream, cpal::Error> {
    let mut faulted = false;
    device.build_output_stream::<f32, _, _>(
        *config,
        move |out: &mut [f32], _| {
            if faulted {
                out.fill(0.0);
                return;
            }
            // Last line of defence (ARCHITECTURE.md §3): a panic must not unwind into the
            // backend. On panic, mute for good and let the UI offer a restart.
            if catch_unwind(AssertUnwindSafe(|| processor.process(out))).is_err() {
                faulted = true;
                out.fill(0.0);
                processor_fault(&processor);
            }
        },
        error_callback(error_slot),
        None,
    )
}

/// Integer or f64 devices: render into f32 scratch, then convert at the edge.
fn build_converted<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut processor: AudioProcessor,
    error_slot: Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels.max(1));
    // Allocated here, on the UI thread, and moved into the callback.
    let mut scratch = vec![0.0_f32; SCRATCH_FRAMES * channels];
    let mut faulted = false;
    device.build_output_stream::<T, _, _>(
        *config,
        move |out: &mut [T], _| {
            if faulted {
                out.fill(T::EQUILIBRIUM);
                return;
            }
            let result = catch_unwind(AssertUnwindSafe(|| {
                for chunk in out.chunks_mut(scratch.len()) {
                    let tmp = &mut scratch[..chunk.len()];
                    processor.process(tmp);
                    for (o, &s) in chunk.iter_mut().zip(tmp.iter()) {
                        *o = T::from_sample(s);
                    }
                }
            }));
            if result.is_err() {
                faulted = true;
                out.fill(T::EQUILIBRIUM);
                processor_fault(&processor);
            }
        },
        error_callback(error_slot),
        None,
    )
}

fn processor_fault(processor: &AudioProcessor) {
    processor.telemetry().faulted.store(true, Ordering::Relaxed);
}
