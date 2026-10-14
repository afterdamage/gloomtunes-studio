//! Video export (Prompt 26, D135): the mix as an MP4 with a still image or an audio-reactive
//! visual, made by an external ffmpeg if one is installed (never bundled: ffmpeg builds with
//! x264 are GPL, and the binary is large).
//!
//! The visuals are drawn here, offline, one RGB frame at a time, and piped to ffmpeg as raw
//! video; the audio goes in as a float WAV file. Two styles: mirrored spectrum bars on a
//! log-frequency axis, and a scrolling min/max waveform.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use crate::{BitDepth, ExportError, ExportSettings, Progress};

/// What the picture shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoStyle {
    /// Spectrum bars, mirrored around the middle.
    Spectrum,
    /// The waveform around the play position.
    Waveform,
    /// A still image (PNG, JPEG...), fitted into the frame.
    Image(PathBuf),
}

/// Video options.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoSettings {
    /// Picture.
    pub style: VideoStyle,
    /// Frame width in pixels (even).
    pub width: u32,
    /// Frame height in pixels (even).
    pub height: u32,
    /// Frames per second.
    pub fps: u32,
    /// Background colour.
    pub background: [u8; 3],
    /// Bar and line colour.
    pub accent: [u8; 3],
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            style: VideoStyle::Spectrum,
            width: 1280,
            height: 720,
            fps: 30,
            background: [18, 17, 22],
            accent: [168, 120, 255],
        }
    }
}

/// The ffmpeg to use: `GT_FFMPEG` if set, else `ffmpeg` on the `PATH`.
pub fn find_ffmpeg() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("GT_FFMPEG").map(PathBuf::from) {
        return p.is_file().then_some(p);
    }
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// Number of bars in the spectrum.
pub const BARS: usize = 48;
const FFT_SIZE: usize = 2048;
const LOW_HZ: f32 = 30.0;
const HIGH_HZ: f32 = 16_000.0;
/// Bar range in dB below full scale.
const FLOOR_DB: f32 = -72.0;

/// Draws the frames of a visual.
pub struct Visualizer {
    mono: Vec<f32>,
    rate: u32,
    settings: VideoSettings,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    buf: Vec<Complex32>,
    /// Bar heights 0 to 1, falling slowly between frames.
    bars: [f32; BARS],
}

impl Visualizer {
    /// A visual of interleaved stereo `audio` at `rate`.
    pub fn new(audio: &[f32], rate: u32, settings: &VideoSettings) -> Self {
        let mono = audio.chunks_exact(2).map(|f| 0.5 * (f[0] + f[1])).collect();
        let window = (0..FFT_SIZE)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / (FFT_SIZE - 1) as f32).cos())
            .collect();
        Self {
            mono,
            rate,
            settings: settings.clone(),
            fft: FftPlanner::new().plan_fft_forward(FFT_SIZE),
            window,
            buf: vec![Complex32::default(); FFT_SIZE],
            bars: [0.0; BARS],
        }
    }

    /// Frames in the video.
    pub fn frames(&self) -> usize {
        let seconds = self.mono.len() as f64 / f64::from(self.rate.max(1));
        (seconds * f64::from(self.settings.fps)).ceil().max(1.0) as usize
    }

    /// Bytes in one RGB frame.
    pub fn frame_bytes(&self) -> usize {
        self.settings.width as usize * self.settings.height as usize * 3
    }

    /// The spectrum at `frame`: bar heights 0 to 1 (with the fall from the frame before).
    pub fn spectrum(&mut self, frame: usize) -> [f32; BARS] {
        let center = (frame as f64 / f64::from(self.settings.fps) * f64::from(self.rate)) as i64;
        let start = center - FFT_SIZE as i64 / 2;
        for (i, c) in self.buf.iter_mut().enumerate() {
            let k = start + i as i64;
            let x = if k >= 0 {
                self.mono.get(k as usize).copied().unwrap_or(0.0)
            } else {
                0.0
            };
            *c = Complex32::new(x * self.window[i], 0.0);
        }
        self.fft.process(&mut self.buf);
        // A full-scale sine reads 0 dB: the Hann window's gain is 1/2.
        let norm = 4.0 / FFT_SIZE as f32;
        let bin_hz = self.rate as f32 / FFT_SIZE as f32;
        let nyquist = FFT_SIZE / 2;
        let fall = 1.5 / self.settings.fps as f32;
        for (b, bar) in self.bars.iter_mut().enumerate() {
            let lo = LOW_HZ * (HIGH_HZ / LOW_HZ).powf(b as f32 / BARS as f32);
            let hi = LOW_HZ * (HIGH_HZ / LOW_HZ).powf((b + 1) as f32 / BARS as f32);
            // Bins whose centre is in the band; the nearest one for narrow low bands.
            let a = ((lo / bin_hz).ceil() as usize).min(nyquist - 1);
            let z = ((hi / bin_hz).ceil() as usize).min(nyquist);
            let (a, z) = if z > a {
                (a, z)
            } else {
                let k = ((lo / bin_hz).round() as usize).min(nyquist - 1);
                (k, k + 1)
            };
            let peak = self.buf[a..z]
                .iter()
                .map(|c| c.norm() * norm)
                .fold(0.0_f32, f32::max);
            let db = 20.0 * peak.max(1e-9).log10();
            let h = ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0);
            *bar = h.max(*bar - fall);
        }
        self.bars
    }

    /// Draws `frame` into `rgb` (`frame_bytes` long). Frames must be drawn in order for the
    /// spectrum's fall to look right.
    pub fn draw(&mut self, frame: usize, rgb: &mut [u8]) {
        let (w, h) = (self.settings.width as usize, self.settings.height as usize);
        let bg = self.settings.background;
        for px in rgb.chunks_exact_mut(3) {
            px.copy_from_slice(&bg);
        }
        let accent = self.settings.accent;
        let mid = h / 2;
        match self.settings.style {
            VideoStyle::Spectrum => {
                let bars = self.spectrum(frame);
                let slot = w as f32 / BARS as f32;
                for (b, &v) in bars.iter().enumerate() {
                    let x0 = (b as f32 * slot + slot * 0.15) as usize;
                    let x1 = ((b + 1) as f32 * slot - slot * 0.15) as usize;
                    let half = (v * (h as f32 * 0.42)) as usize;
                    for y in mid.saturating_sub(half)..(mid + half).min(h) {
                        // Brighter towards the middle.
                        let t = 1.0 - (y as f32 - mid as f32).abs() / (h as f32 * 0.5);
                        let c = mix(bg, accent, 0.45 + 0.55 * t);
                        fill_row(rgb, w, y, x0, x1.min(w), c);
                    }
                }
            }
            VideoStyle::Waveform => {
                // 80 ms either side of the play position.
                let span = (self.rate as f32 * 0.16) as usize;
                let center =
                    (frame as f64 / f64::from(self.settings.fps) * f64::from(self.rate)) as i64;
                let first = center - span as i64 / 2;
                let scale = h as f32 * 0.4;
                for x in 0..w {
                    let a = first + (x * span / w) as i64;
                    let b = first + ((x + 1) * span / w).max(x * span / w + 1) as i64;
                    let (mut lo, mut hi) = (0.0_f32, 0.0_f32);
                    for k in a.max(0)..b.max(0) {
                        let s = self.mono.get(k as usize).copied().unwrap_or(0.0);
                        lo = lo.min(s);
                        hi = hi.max(s);
                    }
                    let y0 = (mid as f32 - hi.clamp(-1.0, 1.0) * scale) as usize;
                    let y1 = (mid as f32 - lo.clamp(-1.0, 1.0) * scale) as usize;
                    for y in y0..=y1.min(h - 1) {
                        let i = (y * w + x) * 3;
                        rgb[i..i + 3].copy_from_slice(&accent);
                    }
                }
            }
            VideoStyle::Image(_) => {}
        }
    }
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    std::array::from_fn(|i| (f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * t) as u8)
}

fn fill_row(rgb: &mut [u8], w: usize, y: usize, x0: usize, x1: usize, c: [u8; 3]) {
    for x in x0..x1 {
        let i = (y * w + x) * 3;
        rgb[i..i + 3].copy_from_slice(&c);
    }
}

fn ffmpeg_error(msg: impl Into<String>) -> ExportError {
    ExportError::Io(std::io::Error::other(msg.into()))
}

/// Writes interleaved stereo `audio` at `rate` as an MP4 at `out` with ffmpeg (H.264 if the
/// ffmpeg has it, else MPEG-4 Part 2; AAC audio).
pub fn export_video(
    audio: &[f32],
    rate: u32,
    settings: &VideoSettings,
    out: &Path,
    progress: &Progress,
) -> Result<(), ExportError> {
    let ffmpeg = find_ffmpeg().ok_or_else(|| {
        ffmpeg_error("video export needs ffmpeg: install it, or set GT_FFMPEG to its path")
    })?;
    let wav = out.with_extension("audio.wav");
    let tmp = out.with_extension("part.mp4");
    crate::write_wav(
        &wav,
        audio,
        &ExportSettings {
            sample_rate: rate,
            depth: BitDepth::Float32,
            dither: false,
            ..ExportSettings::default()
        },
        0,
    )?;
    let result = run_ffmpeg(&ffmpeg, &wav, &tmp, audio, rate, settings, progress);
    let _ = std::fs::remove_file(&wav);
    match result {
        Ok(()) => std::fs::rename(&tmp, out).map_err(ExportError::from),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn run_ffmpeg(
    ffmpeg: &Path,
    wav: &Path,
    tmp: &Path,
    audio: &[f32],
    rate: u32,
    settings: &VideoSettings,
    progress: &Progress,
) -> Result<(), ExportError> {
    let x264 = Command::new(ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(" libx264 "));
    let (w, h) = (settings.width & !1, settings.height & !1);
    let fps = settings.fps.clamp(1, 120).to_string();
    let size = format!("{w}x{h}");
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    match &settings.style {
        VideoStyle::Image(img) => {
            cmd.args(["-loop", "1", "-framerate", &fps, "-i"]).arg(img);
        }
        _ => {
            cmd.args([
                "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", &size, "-r", &fps,
            ]);
            cmd.args(["-i", "pipe:0"]);
        }
    }
    cmd.arg("-i").arg(wav);
    cmd.args(["-map", "0:v", "-map", "1:a"]);
    let fit = format!(
        "scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,\
         format=yuv420p"
    );
    cmd.args(["-vf", &fit]);
    if x264 {
        cmd.args(["-c:v", "libx264", "-crf", "20", "-preset", "medium"]);
        if matches!(settings.style, VideoStyle::Image(_)) {
            cmd.args(["-tune", "stillimage"]);
        }
    } else {
        cmd.args(["-c:v", "mpeg4", "-q:v", "3"]);
    }
    cmd.args([
        "-c:a",
        "aac",
        "-b:a",
        "256k",
        "-shortest",
        "-movflags",
        "+faststart",
    ]);
    cmd.arg(tmp);
    let piped = !matches!(settings.style, VideoStyle::Image(_));
    let mut child = cmd
        .stdin(if piped { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| ffmpeg_error(format!("could not start ffmpeg: {e}")))?;
    // Read ffmpeg's messages on the side, so a full pipe never stalls it.
    let mut stderr = child.stderr.take().expect("piped");
    let log = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let mut cancelled = false;
    if piped {
        let mut stdin = child.stdin.take().expect("piped");
        let mut vis = Visualizer::new(
            audio,
            rate,
            &VideoSettings {
                width: w,
                height: h,
                ..settings.clone()
            },
        );
        let frames = vis.frames();
        let mut rgb = vec![0; vis.frame_bytes()];
        for f in 0..frames {
            if progress.cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            vis.draw(f, &mut rgb);
            if stdin.write_all(&rgb).is_err() {
                break; // ffmpeg stopped: its message says why
            }
            progress
                .permille
                .store((f * 1000 / frames) as u32, Ordering::Relaxed);
        }
        drop(stdin);
    }
    if cancelled {
        let _ = child.kill();
    }
    let status = child.wait()?;
    let log = log.join().unwrap_or_default();
    if cancelled {
        return Err(ExportError::Cancelled);
    }
    if !status.success() {
        let last = log
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        return Err(ffmpeg_error(format!("ffmpeg failed ({status}): {last}")));
    }
    progress.permille.store(1000, Ordering::Relaxed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f32, rate: u32, seconds: f32) -> Vec<f32> {
        (0..(rate as f32 * seconds) as usize)
            .flat_map(|i| {
                let x = 0.5 * (std::f32::consts::TAU * hz * i as f32 / rate as f32).sin();
                [x, x]
            })
            .collect()
    }

    #[test]
    fn spectrum_peaks_at_the_tone() {
        let audio = sine(1000.0, 48_000, 1.0);
        let mut v = Visualizer::new(&audio, 48_000, &VideoSettings::default());
        assert_eq!(v.frames(), 30);
        let bars = v.spectrum(15);
        let top = (0..BARS)
            .max_by(|&a, &b| bars[a].total_cmp(&bars[b]))
            .unwrap();
        let lo = LOW_HZ * (HIGH_HZ / LOW_HZ).powf(top as f32 / BARS as f32);
        let hi = LOW_HZ * (HIGH_HZ / LOW_HZ).powf((top + 1) as f32 / BARS as f32);
        assert!(lo <= 1000.0 && 1000.0 < hi, "{lo}..{hi}");
        // -6 dB sine: about 0.92 of the range.
        assert!(
            (bars[top] - (1.0 - 6.0 / 72.0)).abs() < 0.03,
            "{}",
            bars[top]
        );
        assert!(bars[0] < 0.2 && bars[BARS - 1] < 0.2);
    }

    #[test]
    fn silence_draws_the_background_and_bars_fall() {
        let mut audio = sine(200.0, 48_000, 0.5);
        audio.extend(vec![0.0; 2 * 48_000]);
        let s = VideoSettings {
            width: 64,
            height: 36,
            ..VideoSettings::default()
        };
        let mut v = Visualizer::new(&audio, 48_000, &s);
        let mut rgb = vec![0; v.frame_bytes()];
        v.draw(5, &mut rgb);
        assert!(rgb.chunks(3).any(|p| p != s.background));
        let peak = v.bars.iter().copied().fold(0.0, f32::max);
        v.draw(17, &mut rgb);
        let after = v.bars.iter().copied().fold(0.0, f32::max);
        assert!(
            after < peak && after > 0.0,
            "falls, not drops: {peak} {after}"
        );
        for f in 18..v.frames() {
            v.draw(f, &mut rgb);
        }
        assert_eq!(v.bars, [0.0; BARS]);
        assert!(rgb.chunks(3).all(|p| p == s.background));
        let mut w = Visualizer::new(
            &audio,
            48_000,
            &VideoSettings {
                style: VideoStyle::Waveform,
                ..s.clone()
            },
        );
        w.draw(5, &mut rgb);
        let lit = rgb.chunks(3).filter(|p| *p == s.accent).count();
        assert!(lit > 64, "{lit}");
    }
}
