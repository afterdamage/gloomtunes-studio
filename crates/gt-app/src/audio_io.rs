//! Audio device layer: the only code that talks to cpal.
//!
//! Lives on the UI thread. It enumerates devices, keeps one output stream open around an
//! `AudioProcessor`, converts samples at the edge, and turns device errors into status text.
//! Since Prompt 13 it also opens an input stream (at the output's sample rate) that feeds the
//! engine through a lock-free ring, and reports both devices' latencies into the telemetry.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    ErrorKind, FromSample, SampleFormat, SizedSample, StreamConfig, SupportedBufferSize,
    SupportedStreamConfig,
};
use gt_core::{AudioInput, MonitorMode, MAX_INPUT_CHANNELS};
use gt_engine::{
    AudioProcessor, EngineCommand, EngineConfig, EngineHandle, InputFeed, LoadMeter, Telemetry,
};
use gt_ui::views::PairMode;

use crate::settings::AudioPrefs;

/// Sample rates offered when the device supports them.
const STANDARD_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

/// Frames of f32 scratch for devices that want integer samples. Larger callbacks are rendered
/// in several chunks, so this only bounds the chunk size, not the device buffer size.
const SCRATCH_FRAMES: usize = 4096;
/// If the fade-out has not been confirmed by the audio thread after this long (e.g. the device
/// stopped calling back), the stream is reopened anyway.
const STOP_TIMEOUT: Duration = Duration::from_millis(300);

/// The open input stream.
struct InputRunning {
    stream: cpal::Stream,
    channels: usize,
    device_name: String,
}

impl Drop for InputRunning {
    fn drop(&mut self) {
        let _ = self.stream.pause();
    }
}

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
    /// Requested sample rate; `None` uses the device's own default.
    sample_rate: Option<u32>,
    /// Standard rates the selected device supports.
    rates: Vec<u32>,
    running: Option<Running>,
    /// Set while the output fades out before the stream is reopened.
    reopening_since: Option<Instant>,
    /// True once after a new engine was created, so the app can send it the current settings.
    fresh_engine: bool,
    /// Engine events since the app last took them.
    events: Vec<gt_engine::EngineEvent>,
    stream_error: Arc<Mutex<Option<String>>>,
    /// The backend could not give the audio thread real-time priority.
    rt_denied: Arc<AtomicBool>,
    status: String,

    /// Input devices; `None` at index 0 means "no input".
    in_devices: Vec<Option<cpal::Device>>,
    in_names: Vec<String>,
    in_selected: usize,
    input: Option<InputRunning>,
    in_status: String,
    /// How each channel pair is offered.
    pub pairs: Vec<PairMode>,
    /// Monitoring of armed tracks.
    pub monitor: MonitorMode,
    /// Manual latency offset in ms.
    pub offset_ms: f32,
}

impl AudioIo {
    /// Opens the output with the saved preferences (device by name, buffer size, rate).
    pub fn new(prefs: &AudioPrefs) -> Self {
        let host = pick_host();
        log::info!("audio host: {}", host.id().name());
        let mut io = Self {
            host,
            devices: Vec::new(),
            names: vec!["System default".to_owned()],
            selected: 0,
            buffer_size: prefs.buffer_size,
            sample_rate: prefs.sample_rate,
            rates: Vec::new(),
            running: None,
            reopening_since: None,
            fresh_engine: false,
            events: Vec::new(),
            stream_error: Arc::new(Mutex::new(None)),
            rt_denied: Arc::new(AtomicBool::new(false)),
            status: String::new(),
            in_devices: vec![None],
            in_names: vec![NO_INPUT.to_owned()],
            in_selected: 0,
            input: None,
            in_status: String::new(),
            pairs: prefs
                .input_pairs
                .iter()
                .map(|k| PairMode::from_key(k))
                .collect(),
            monitor: MonitorMode::from_key(&prefs.monitor),
            offset_ms: if prefs.input_offset_ms.is_finite() {
                prefs.input_offset_ms.clamp(-200.0, 200.0)
            } else {
                0.0
            },
        };
        if let Some(name) = &prefs.device {
            // rescan keeps the selection by name.
            io.names.push(name.clone());
            io.selected = 1;
        }
        if let Some(name) = &prefs.input_device {
            io.in_names.push(name.clone());
            io.in_selected = 1;
        }
        io.rescan();
        io.open();
        io
    }

    /// The settings to save: device by name (none: system default), buffer size and rate.
    pub fn prefs(&self) -> AudioPrefs {
        AudioPrefs {
            device: (self.selected > 0)
                .then(|| self.names.get(self.selected).cloned())
                .flatten(),
            buffer_size: self.buffer_size,
            sample_rate: self.sample_rate,
            input_device: (self.in_selected > 0)
                .then(|| self.in_names.get(self.in_selected).cloned())
                .flatten(),
            input_pairs: self.pairs.iter().map(|p| p.key().to_owned()).collect(),
            monitor: self.monitor.key().to_owned(),
            input_offset_ms: self.offset_ms,
        }
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
        self.rates = self.device().map_or_else(Vec::new, |d| supported_rates(&d));

        let previous = self.in_names.get(self.in_selected).cloned();
        self.in_devices = vec![None];
        self.in_names = vec![NO_INPUT.to_owned()];
        if let Ok(devices) = self.host.input_devices() {
            for d in devices {
                self.in_names.push(device_name(&d));
                self.in_devices.push(Some(d));
            }
        }
        self.in_selected = previous
            .and_then(|p| self.in_names.iter().position(|n| *n == p))
            .unwrap_or(0);
    }

    /// Picks the input device (0: none) and opens it.
    pub fn select_input(&mut self, index: usize) {
        if index < self.in_devices.len() {
            self.in_selected = index;
            self.open_input();
        }
    }

    /// Input device names (index 0: "No input") and the selection.
    pub fn input_devices(&self) -> (&[String], usize) {
        (&self.in_names, self.in_selected)
    }

    /// Channels of the open input stream (0 without one).
    pub fn input_channels(&self) -> usize {
        self.input.as_ref().map_or(0, |i| i.channels)
    }

    /// Inputs tracks and inserts can use, as the channel pairs are set up.
    pub fn offered_inputs(&self) -> Vec<AudioInput> {
        gt_ui::views::offered_inputs(self.input_channels(), &self.pairs)
    }

    /// Status of the input (why it is not open, or what is open).
    pub fn input_status(&self) -> &str {
        &self.in_status
    }

    /// True when input and output are different devices (and so have separate clocks).
    pub fn separate_clocks(&self) -> bool {
        match (&self.input, &self.running) {
            (Some(i), Some(r)) => !r.device_name.starts_with(&i.device_name),
            _ => false,
        }
    }

    /// The latency the devices report for a round trip, in frames: output plus input buffers
    /// plus the input frames the engine keeps waiting, plus one render quantum.
    pub fn reported_latency(&self) -> Option<u32> {
        let e = self.engine()?;
        self.input.as_ref()?;
        let t = e.telemetry();
        let fill = t.input_fill.load(Ordering::Relaxed).max(0.0).round() as u32;
        Some(
            t.output_latency.load(Ordering::Relaxed)
                + t.input_latency.load(Ordering::Relaxed)
                + fill
                + gt_engine::RENDER_QUANTUM as u32,
        )
    }

    /// The compensation for recordings, in frames: reported latency plus the manual offset.
    pub fn compensation(&self) -> u32 {
        let sr = self.engine().map_or(48_000, |e| e.config().sample_rate) as f32;
        let reported = self.reported_latency().unwrap_or(0) as f32;
        (reported + self.offset_ms * sr / 1000.0).round().max(0.0) as u32
    }

    /// Opens the selected input for the running engine (closing the previous one).
    fn open_input(&mut self) {
        self.input = None;
        let Some(r) = self.running.as_mut() else {
            self.in_status = String::new();
            return;
        };
        let _ = r.engine.send(EngineCommand::SetInput(None));
        let device = match self.in_devices.get(self.in_selected) {
            Some(Some(d)) => d.clone(),
            _ => {
                self.in_status = if self.in_selected == 0 {
                    String::new()
                } else {
                    "The input device is not connected".to_owned()
                };
                return;
            }
        };
        match open_input_stream(&device, r, self.buffer_size) {
            Ok((running, feed)) => {
                self.in_status = format!(
                    "Input: {} channels from {}",
                    running.channels, running.device_name
                );
                let _ = r.engine.send(EngineCommand::SetInput(Some(Box::new(feed))));
                self.input = Some(running);
            }
            Err(e) => {
                log::warn!("{e}");
                self.in_status = e;
            }
        }
    }

    pub fn select_device(&mut self, index: usize) {
        if index < self.devices.len() {
            self.selected = index;
            self.rates = self.device().map_or_else(Vec::new, |d| supported_rates(&d));
            self.reopen();
        }
    }

    /// Picks a sample rate (`None`: the device's default) and reopens the stream.
    pub fn select_sample_rate(&mut self, rate: Option<u32>) {
        self.sample_rate = rate;
        self.reopen();
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
            self.input = None;
            self.running = None;
            self.reopening_since = None;
            return;
        }
        if let Some(since) = self.reopening_since {
            let silent = r.engine.telemetry().silent.load(Ordering::Relaxed);
            if silent || since.elapsed() > STOP_TIMEOUT {
                self.input = None;
                self.running = None; // dropping the stream closes the device
                self.reopening_since = None;
                self.open();
            }
        }
    }

    /// Load figures for the meter: smoothed load, worst callback since the last call (reset
    /// on read), overload and xrun counts. `None` without a stream.
    pub fn load(&self) -> Option<gt_ui::views::CpuLoad> {
        let t = self.engine()?.telemetry();
        Some(gt_ui::views::CpuLoad {
            load: t.cpu_load.load(Ordering::Relaxed),
            peak: t.cpu_peak.swap(0.0, Ordering::Relaxed),
            overloads: t.overloads.load(Ordering::Relaxed),
            xruns: t.xruns.load(Ordering::Relaxed),
        })
    }

    /// True when the backend reported that the audio thread could not get real-time
    /// priority (see README: the `audio` group on Linux).
    pub fn realtime_denied(&self) -> bool {
        self.running.is_some() && self.rt_denied.load(Ordering::Relaxed)
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
            sample_rates: self.rates.clone(),
            requested_rate: self.sample_rate,
            exclusive_note: cfg!(windows).then(|| {
                "WASAPI exclusive mode is not available yet: the audio library GloomTunes uses \
                 (cpal) only opens shared-mode streams. For the lowest latency on Windows, use \
                 an ASIO build (see README)."
                    .to_owned()
            }),
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
                self.open_input();
            }
            Err(e) => {
                log::error!("{e}");
                self.status = e;
            }
        }
    }

    fn open_stream(&mut self) -> Result<Running, String> {
        let device = self.device().ok_or("No output device available")?;
        let default = device
            .default_output_config()
            .map_err(|e| format!("Device has no usable output config: {e}"))?;
        let mut rate_note = String::new();
        let supported = match self.sample_rate {
            Some(rate) if rate != default.sample_rate() => {
                pick_config(&device, rate, default.channels()).unwrap_or_else(|| {
                    rate_note = format!(" ({rate} Hz not supported; using the device default)");
                    default
                })
            }
            _ => default,
        };
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
        self.rt_denied.store(false, Ordering::Relaxed);
        let error_slot = ErrorSink {
            slot: Arc::clone(&self.stream_error),
            rt_denied: Arc::clone(&self.rt_denied),
            telemetry: engine.shared_telemetry(),
        };
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
            device_name: format!("{}{note}{rate_note}", device_name(&device)),
        })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // Pausing first makes the backend stop calling back before the stream is torn down.
        let _ = self.stream.pause();
    }
}

const NO_INPUT: &str = "No input";

/// Opens `device` for input at the output's sample rate, feeding a new ring for the engine.
fn open_input_stream(
    device: &cpal::Device,
    out: &Running,
    buffer_size: u32,
) -> Result<(InputRunning, InputFeed), String> {
    let rate = out.sample_rate;
    let name = device_name(device);
    let mut configs: Vec<_> = device
        .supported_input_configs()
        .map_err(|e| format!("{name} has no usable input: {e}"))?
        .filter(|c| (c.min_sample_rate()..=c.max_sample_rate()).contains(&rate))
        .collect();
    // The device's own channel count (its default input config), then the most channels up
    // to what the engine reads, then f32 samples.
    let default_channels = device.default_input_config().ok().map(|c| c.channels());
    configs.sort_by_key(|c| {
        (
            Some(c.channels()) != default_channels,
            std::cmp::Reverse(usize::from(c.channels()).min(MAX_INPUT_CHANNELS)),
            c.sample_format() != SampleFormat::F32,
        )
    });
    let supported = configs
        .into_iter()
        .next()
        .and_then(|c| c.try_with_sample_rate(rate))
        .ok_or_else(|| {
            format!(
                "{name} cannot record at {rate} Hz (the output's rate); pick the same rate for \
                 both or another input"
            )
        })?;
    let format = supported.sample_format();
    let mut config: StreamConfig = supported.config();
    if let SupportedBufferSize::Range { min, max } = supported.buffer_size() {
        if (*min..=*max).contains(&buffer_size) {
            config.buffer_size = cpal::BufferSize::Fixed(buffer_size);
        }
    }
    let channels = usize::from(config.channels.max(1));
    // Half a second of input; the engine keeps about one buffer of it waiting.
    let (tx, rx) = rtrb::RingBuffer::new(rate as usize / 2 * channels);
    let telemetry = out.engine.shared_telemetry();
    let error_slot = ErrorSink {
        slot: Arc::new(Mutex::new(None)),
        rt_denied: Arc::new(AtomicBool::new(false)),
        telemetry: Arc::clone(&telemetry),
    };
    let stream = match format {
        SampleFormat::F32 => build_input::<f32>(device, &config, tx, telemetry, error_slot),
        SampleFormat::F64 => build_input::<f64>(device, &config, tx, telemetry, error_slot),
        SampleFormat::I16 => build_input::<i16>(device, &config, tx, telemetry, error_slot),
        SampleFormat::I32 => build_input::<i32>(device, &config, tx, telemetry, error_slot),
        SampleFormat::U16 => build_input::<u16>(device, &config, tx, telemetry, error_slot),
        SampleFormat::U8 => build_input::<u8>(device, &config, tx, telemetry, error_slot),
        SampleFormat::I8 => build_input::<i8>(device, &config, tx, telemetry, error_slot),
        other => return Err(format!("Unsupported input sample format {other}")),
    }
    .map_err(|e| format!("Could not open the input of {name}: {e}"))?;
    stream
        .play()
        .map_err(|e| format!("Could not start the input of {name}: {e}"))?;
    log::info!("input open: {name}, {rate} Hz, {channels} ch, {format}");
    let prime = (buffer_size as usize).max(gt_engine::RENDER_QUANTUM);
    let feed = InputFeed {
        samples: rx,
        channels,
        prime_frames: prime,
        max_frames: prime * 4,
    };
    Ok((
        InputRunning {
            stream,
            channels,
            device_name: name,
        },
        feed,
    ))
}

/// The input callback: converts to f32 and pushes into the ring (dropping what does not fit;
/// the engine notices the gap), and reports the capture latency.
fn build_input<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut tx: rtrb::Producer<f32>,
    telemetry: Arc<Telemetry>,
    error_slot: ErrorSink,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let rate = f64::from(config.sample_rate);
    device.build_input_stream::<T, _, _>(
        *config,
        move |data: &[T], info: &cpal::InputCallbackInfo| {
            let ts = info.timestamp();
            let latency = ts.callback.duration_since(ts.capture).as_secs_f64() * rate;
            telemetry
                .input_latency
                .store(latency.round() as u32, Ordering::Relaxed);
            let n = data.len().min(tx.slots());
            if let Ok(chunk) = tx.write_chunk_uninit(n) {
                chunk.fill_from_iter(
                    data[..n]
                        .iter()
                        .map(|&s| <f32 as FromSample<T>>::from_sample_(s)),
                );
            }
        },
        error_callback(error_slot),
        None,
    )
}

/// Stores the output latency the backend reports for this callback.
fn report_output_latency(telemetry: &Telemetry, info: &cpal::OutputCallbackInfo, rate: f64) {
    let ts = info.timestamp();
    let latency = ts.playback.duration_since(ts.callback).as_secs_f64() * rate;
    telemetry
        .output_latency
        .store(latency.round() as u32, Ordering::Relaxed);
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

/// Where stream errors go: xruns are counted in the telemetry, a refused real-time priority
/// is flagged, anything else becomes the status line.
struct ErrorSink {
    slot: Arc<Mutex<Option<String>>>,
    rt_denied: Arc<AtomicBool>,
    telemetry: Arc<Telemetry>,
}

fn error_callback(sink: ErrorSink) -> impl FnMut(cpal::Error) + Send + 'static {
    // Some backends (ALSA) call this on the audio thread itself, so it never blocks: an xrun
    // is one atomic add, and other errors are dropped if the UI holds the slot right now.
    move |err| {
        if err.kind() == ErrorKind::Xrun {
            sink.telemetry.xruns.fetch_add(1, Ordering::Relaxed);
        } else if err.kind() == ErrorKind::RealtimeDenied {
            // Audio still plays; the performance page explains it.
            sink.rt_denied.store(true, Ordering::Relaxed);
        } else if let Ok(mut s) = sink.slot.try_lock() {
            *s = Some(format!("Audio device: {err}"));
        }
    }
}

/// Standard rates `device` can output.
fn supported_rates(device: &cpal::Device) -> Vec<u32> {
    let Ok(configs) = device.supported_output_configs() else {
        return Vec::new();
    };
    let ranges: Vec<_> = configs
        .map(|c| (c.min_sample_rate(), c.max_sample_rate()))
        .collect();
    STANDARD_RATES
        .into_iter()
        .filter(|r| ranges.iter().any(|(lo, hi)| (lo..=hi).contains(&r)))
        .collect()
}

/// A config at `rate`, preferring the default channel count and f32 samples.
fn pick_config(device: &cpal::Device, rate: u32, channels: u16) -> Option<SupportedStreamConfig> {
    let mut configs: Vec<_> = device
        .supported_output_configs()
        .ok()?
        .filter(|c| (c.min_sample_rate()..=c.max_sample_rate()).contains(&rate))
        .collect();
    configs.sort_by_key(|c| {
        (
            c.channels() != channels,
            c.sample_format() != SampleFormat::F32,
        )
    });
    configs.into_iter().next()?.try_with_sample_rate(rate)
}

/// f32 devices: the engine renders straight into the device buffer.
fn build_f32(
    device: &cpal::Device,
    config: &StreamConfig,
    mut processor: AudioProcessor,
    error_slot: ErrorSink,
) -> Result<cpal::Stream, cpal::Error> {
    let mut faulted = false;
    let channels = usize::from(config.channels.max(1));
    let rate = f64::from(config.sample_rate);
    let mut meter = LoadMeter::new(config.sample_rate);
    device.build_output_stream::<f32, _, _>(
        *config,
        move |out: &mut [f32], info: &cpal::OutputCallbackInfo| {
            if faulted {
                out.fill(0.0);
                return;
            }
            let start = Instant::now();
            report_output_latency(processor.telemetry(), info, rate);
            // Last line of defence (ARCHITECTURE.md §3): a panic must not unwind into the
            // backend. On panic, mute for good and let the UI offer a restart.
            if catch_unwind(AssertUnwindSafe(|| processor.process(out))).is_err() {
                faulted = true;
                out.fill(0.0);
                processor_fault(&processor);
                return;
            }
            let ns = start.elapsed().as_nanos() as u64;
            meter.record(processor.telemetry(), ns, out.len() / channels);
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
    error_slot: ErrorSink,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels.max(1));
    // Allocated here, on the UI thread, and moved into the callback.
    let mut scratch = vec![0.0_f32; SCRATCH_FRAMES * channels];
    let mut faulted = false;
    let rate = f64::from(config.sample_rate);
    let mut meter = LoadMeter::new(config.sample_rate);
    device.build_output_stream::<T, _, _>(
        *config,
        move |out: &mut [T], info: &cpal::OutputCallbackInfo| {
            if faulted {
                out.fill(T::EQUILIBRIUM);
                return;
            }
            let start = Instant::now();
            report_output_latency(processor.telemetry(), info, rate);
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
                return;
            }
            let ns = start.elapsed().as_nanos() as u64;
            meter.record(processor.telemetry(), ns, out.len() / channels);
        },
        error_callback(error_slot),
        None,
    )
}

fn processor_fault(processor: &AudioProcessor) {
    processor.telemetry().faulted.store(true, Ordering::Relaxed);
}
