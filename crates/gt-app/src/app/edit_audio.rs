//! The audio editor window, the app side (Prompt 32, D146): opens a clip's loaded sound as a
//! working copy, saves an applied copy next to the project's recordings and points the clip at
//! it (one undo step; the original file stays), and turns audio into a pattern.

use gt_core::audio_midi::TimedNote;
use gt_core::{ClipKind, SampleSource};
use gt_dsp::audio_to_midi;
use gt_export::{BitDepth, ExportSettings};
use gt_ui::views::{audio_editor, AudioEditor, AudioEditorAction};
use gt_ui::{tr, GloomTheme};

use super::GloomApp;
use crate::library::data_folder;
use crate::recording;

impl GloomApp {
    /// Opens the editor the playlist asked for, and draws it.
    pub(super) fn audio_editor_window(&mut self, ctx: &egui::Context, theme: &GloomTheme) {
        if let Some(id) = self.playlist.edit_audio.take() {
            self.open_audio_editor(id);
        }
        let Some(ed) = self.audio_editor.as_mut() else {
            return;
        };
        let mut open = true;
        let mut action = None;
        egui::Window::new(format!("{} · {}", tr("Audio editor"), ed.name))
            .id(egui::Id::new("audio_editor"))
            .open(&mut open)
            .default_size([760.0, 380.0])
            .show(ctx, |ui| {
                action = audio_editor(ui, theme, ed);
            });
        if !open {
            action = Some(AudioEditorAction::Close);
        }
        match action {
            Some(AudioEditorAction::Close) => self.audio_editor = None,
            Some(AudioEditorAction::Apply) => self.apply_audio_edit(),
            Some(AudioEditorAction::ToMidi { drums }) => self.audio_to_midi(drums),
            None => {}
        }
    }

    fn open_audio_editor(&mut self, id: gt_core::ClipId) {
        let Some(ClipKind::Audio { source, .. }) = self.project.playlist.clip(id).map(|c| &c.kind)
        else {
            return;
        };
        let Some(loaded) = self.library.get(source) else {
            self.toast(tr("The sound is still loading"));
            return;
        };
        let data = &loaded.data;
        self.audio_editor = Some(AudioEditor::new(
            id,
            source.display_name(),
            data.sample_rate,
            data.channels.clone(),
        ));
    }

    /// Saves the working copy as a new file and points the clip at it.
    fn apply_audio_edit(&mut self) {
        let Some(ed) = self.audio_editor.as_ref() else {
            return;
        };
        let (id, rate) = (ed.clip, ed.rate);
        let Some(clip) = self.project.playlist.clip(id) else {
            self.toast(tr("The clip was deleted"));
            self.audio_editor = None;
            return;
        };
        let old_frames = match &clip.kind {
            ClipKind::Audio { source, .. } => self.library.get(source).map(|l| l.data.frames()),
            _ => None,
        };
        let frames = ed.frames();
        let left = &ed.audio[0];
        let right = ed.audio.get(1).unwrap_or(left);
        let interleaved: Vec<f32> = left.iter().zip(right).flat_map(|(&l, &r)| [l, r]).collect();
        let dir = recording::folder(self.doc_path.as_deref(), &data_folder());
        let base = ed
            .name
            .rsplit_once('.')
            .map_or(ed.name.as_str(), |(b, _)| b)
            .to_owned();
        let path = (1..)
            .map(|n| dir.join(format!("{base} edit {n}.wav")))
            .find(|p| !p.exists())
            .unwrap_or_else(|| dir.join(format!("{base} edit.wav")));
        let settings = ExportSettings {
            sample_rate: rate,
            depth: BitDepth::Float32,
            dither: false,
            ..ExportSettings::default()
        };
        let written = std::fs::create_dir_all(&dir)
            .map_err(|e| e.to_string())
            .and_then(|()| {
                gt_export::write_wav(&path, &interleaved, &settings, 0).map_err(|e| e.to_string())
            });
        if let Err(e) = written {
            self.toast(format!("Could not save the edit: {e}"));
            return;
        }
        let src = SampleSource::File(path);
        self.project
            .playlist
            .replace_source(&[id], None, Some(&src));
        // A different length plays from the start of the new sound, all of it.
        if old_frames != Some(frames) {
            let start = self.project.playlist.clip(id).map_or(0, |c| c.start);
            let len = gt_export::bounce::ticks_for(&self.project, start, frames, rate);
            if let Some(c) = self.project.playlist.clip_mut(id) {
                c.offset = 0;
                c.length = len;
            }
        }
        let engine_rate = self.rate();
        self.library.request(&src, engine_rate);
        self.pending_edit = Some("Edit audio");
        self.push_song();
        self.audio_editor = None;
        self.toast(tr("Audio edit applied"));
    }

    /// Turns the editor's selection (or all of it) into a pattern on a new track.
    fn audio_to_midi(&mut self, drums: bool) {
        let Some(ed) = self.audio_editor.as_ref() else {
            return;
        };
        let mono = ed.mono();
        let offset_s = ed.range().start as f64 / f64::from(ed.rate);
        let found = if drums {
            audio_to_midi::drums(&mono, ed.rate, 0.5)
        } else {
            audio_to_midi::melody(&mono, ed.rate)
        };
        let notes: Vec<TimedNote> = found
            .iter()
            .map(|n| TimedNote {
                start_s: offset_s + n.start,
                length_s: n.length,
                key: n.key,
                velocity: n.velocity,
            })
            .collect();
        let name = format!("{} MIDI", ed.name);
        let Some(clip) = self.project.playlist.clip(ed.clip) else {
            return;
        };
        // The sound's first frame plays where the clip's source starts.
        let at = clip.start - clip.offset;
        let Some(channel) = self.project.channels.get(self.rack.selected).map(|c| c.id) else {
            return;
        };
        match self.project.notes_from_audio(&name, channel, at, &notes) {
            Some(_) => {
                self.toast(format!("{} notes found", notes.len()));
                self.pending_edit = Some("Audio to MIDI");
                self.push_song();
            }
            None => self.toast(tr("No notes found")),
        }
    }
}
