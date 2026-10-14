//! Render to audio clip, consolidate and freeze, the app side (Prompt 14, D105): the render
//! runs on a worker thread through `gt_export::bounce`, the file goes to the project's
//! recordings folder (so saving adopts and embeds it like a take), and the result is applied
//! as one undo step.

use gt_core::{ClipId, SampleSource, TrackId};
use gt_export::bounce::{self, Bounce};
use gt_export::{BitDepth, ExportSettings};
use gt_ui::views::BounceKind;
use std::path::{Path, PathBuf};

use super::GloomApp;
use crate::library::data_folder;
use crate::recording;
use gt_ui::tr;

/// A render in progress.
pub(super) struct BounceJob {
    kind: BounceKind,
    bounce: Bounce,
    rate: u32,
    thread: std::thread::JoinHandle<Result<(PathBuf, usize), String>>,
}

/// `<dir>/<name> <what> <n>.wav` with the first `n` not in use.
fn unique(dir: &Path, name: &str, what: &str) -> PathBuf {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let base = match clean.trim() {
        "" => "Track",
        s => s,
    };
    (1..)
        .map(|n| dir.join(format!("{base} {what} {n}.wav")))
        .find(|p| !p.exists())
        .unwrap_or_else(|| dir.join(format!("{base} {what}.wav")))
}

impl GloomApp {
    pub(super) fn on_bounce(&mut self, kind: BounceKind, track: TrackId, clips: Vec<ClipId>) {
        if kind == BounceKind::Unfreeze {
            if let Some(t) = self.project.playlist.track_mut(track) {
                t.frozen = None;
                self.pending_edit = Some("Unfreeze track");
                self.push_song();
            }
            return;
        }
        if self.export_job.is_some() || self.bounce_job.is_some() {
            self.toast(tr("Wait for the current render to finish"));
            return;
        }
        if let Err(e) = bounce::check_routing(&self.project, track) {
            self.toast(e);
            return;
        }
        let t = &self.transport;
        let range = if t.loop_enabled && t.loop_end > t.loop_start {
            (t.loop_start.0, t.loop_end.0)
        } else {
            (0, self.project.playlist.song_end())
        };
        let job = match kind {
            BounceKind::Freeze => bounce::freeze_job(&self.project, track),
            BounceKind::Consolidate => bounce::consolidate_job(&self.project, &clips),
            BounceKind::Render => bounce::render_job(&self.project, track, &clips, Some(range)),
            BounceKind::Unfreeze => None,
        };
        let Some(job) = job else {
            self.toast(tr("Nothing to render"));
            return;
        };
        let name = self
            .project
            .playlist
            .track(track)
            .map_or_else(String::new, |t| t.name.clone());
        let what = match kind {
            BounceKind::Freeze => "freeze",
            BounceKind::Consolidate => "consolidated",
            _ => "render",
        };
        let dir = recording::folder(self.doc_path.as_deref(), &data_folder());
        let rate = self.rate();
        let project = self.project.clone();
        let mut plugins = self.plugins.export_processors(&project, rate);
        let b = job.clone();
        let thread = std::thread::spawn(move || {
            let (samples, _) = gt_export::load_samples(&project, rate);
            let progress = gt_export::Progress::default();
            let mut audio = bounce::render(&project, &samples, &b, rate, &progress, &mut plugins)
                .map_err(|e| e.to_string())?;
            if kind == BounceKind::Freeze {
                bounce::trim_silence(&mut audio);
            }
            if audio.is_empty() {
                return Err("the track is silent".to_owned());
            }
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let path = unique(&dir, &name, what);
            let settings = ExportSettings {
                sample_rate: rate,
                depth: BitDepth::Float32,
                dither: false,
                ..ExportSettings::default()
            };
            gt_export::write_wav(&path, &audio, &settings, 0).map_err(|e| e.to_string())?;
            Ok((path, audio.len() / 2))
        });
        self.bounce_job = Some(BounceJob {
            kind,
            bounce: job,
            rate,
            thread,
        });
        self.toast(match kind {
            BounceKind::Freeze => "Freezing…",
            BounceKind::Consolidate => "Consolidating…",
            _ => "Rendering…",
        });
    }

    /// Applies a finished render.
    pub(super) fn poll_bounce(&mut self) {
        if !self
            .bounce_job
            .as_ref()
            .is_some_and(|j| j.thread.is_finished())
        {
            return;
        }
        let Some(job) = self.bounce_job.take() else {
            return;
        };
        self.plugins.finish_export();
        let result = job
            .thread
            .join()
            .unwrap_or_else(|_| Err("the render crashed".to_owned()));
        let (path, frames) = match result {
            Ok(r) => r,
            Err(e) => {
                self.toast(format!("Render failed: {e}"));
                return;
            }
        };
        let src = SampleSource::File(path);
        let b = &job.bounce;
        let len = bounce::ticks_for(&self.project, b.start, frames, job.rate);
        let (ok, edit) = match job.kind {
            BounceKind::Freeze => (
                bounce::apply_freeze(&mut self.project, b.track, src.clone()),
                "Freeze track",
            ),
            BounceKind::Consolidate => (
                bounce::apply_consolidate(&mut self.project, b, src.clone(), len).is_some(),
                "Consolidate clips",
            ),
            _ => (
                bounce::apply_render(&mut self.project, b, src.clone(), len).is_some(),
                "Render to audio clip",
            ),
        };
        if !ok {
            self.toast(tr("The track was deleted while rendering"));
            return;
        }
        let rate = self.rate();
        self.library.request(&src, rate);
        self.pending_edit = Some(edit);
        self.push_song();
        self.toast(format!("{edit}: done"));
    }
}
