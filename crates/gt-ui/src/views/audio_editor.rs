//! The audio editor (Prompt 32, D146): a window on one audio clip's sound with a zoomable
//! waveform or spectrogram, a selection, and destructive edits on a working copy with undo.
//! Nothing touches the clip until **Apply**, which saves the copy as a new file and points
//! the clip at it (the original file is kept).

use egui::{pos2, vec2, Color32, ColorImage, Rect, RichText, Sense, Stroke, TextureOptions, Ui};
use gt_core::ClipId;
use gt_dsp::audio_edit::{self, Planar};
use gt_dsp::denoise::NoiseProfile;
use spec::spectrogram;

use crate::i18n::tr;
use crate::theme::GloomTheme;

/// Undo steps kept.
const MAX_UNDO: usize = 32;

/// What the app must do for the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioEditorAction {
    /// Save the working copy and point the clip at it.
    Apply,
    /// Close without applying.
    Close,
    /// Turn the selection (or everything) into a pattern: a melody, or drum hits.
    ToMidi {
        /// Drum mode.
        drums: bool,
    },
}

/// An open editor.
pub struct AudioEditor {
    /// The clip being edited.
    pub clip: ClipId,
    /// Shown in the title.
    pub name: String,
    /// Sample rate of `audio`.
    pub rate: u32,
    /// The working copy.
    pub audio: Planar,
    undo: Vec<Planar>,
    redo: Vec<Planar>,
    /// Selected frames, start <= end.
    pub selection: Option<(usize, usize)>,
    /// Where paste goes.
    pub cursor: usize,
    /// Frames per point, and the first frame shown.
    zoom: f64,
    scroll: f64,
    clipboard: Option<Planar>,
    noise: Option<NoiseProfile>,
    reduction_db: f32,
    gain_db: f32,
    /// Show the spectrogram instead of the waveform.
    pub spectrogram: bool,
    spec: Option<((u64, u64, u64), egui::TextureHandle)>,
    version: u64,
    drag_from: Option<usize>,
}

impl AudioEditor {
    /// An editor on `audio` (planar, at `rate`) for `clip`.
    pub fn new(clip: ClipId, name: String, rate: u32, audio: Planar) -> Self {
        Self {
            clip,
            name,
            rate: rate.max(1),
            audio,
            undo: Vec::new(),
            redo: Vec::new(),
            selection: None,
            cursor: 0,
            zoom: 0.0,
            scroll: 0.0,
            clipboard: None,
            noise: None,
            reduction_db: 24.0,
            gain_db: 3.0,
            spectrogram: false,
            spec: None,
            version: 0,
            drag_from: None,
        }
    }

    /// Length in frames.
    pub fn frames(&self) -> usize {
        self.audio.first().map_or(0, Vec::len)
    }

    /// True when there is something to apply.
    pub fn changed(&self) -> bool {
        !self.undo.is_empty()
    }

    /// The selection, or everything.
    pub fn range(&self) -> std::ops::Range<usize> {
        match self.selection {
            Some((a, b)) if b > a => a..b,
            _ => 0..self.frames(),
        }
    }

    /// The selection (or everything) mixed to mono, for analysis.
    pub fn mono(&self) -> Vec<f32> {
        let r = self.range();
        let n = self.audio.len().max(1) as f32;
        (r.clone())
            .map(|i| self.audio.iter().map(|c| c[i]).sum::<f32>() / n)
            .collect()
    }

    /// Runs an edit, keeping the copy before it for undo.
    pub fn edit(&mut self, f: impl FnOnce(&mut Planar)) {
        let before = self.audio.clone();
        f(&mut self.audio);
        if self.audio != before {
            self.undo.push(before);
            if self.undo.len() > MAX_UNDO {
                self.undo.remove(0);
            }
            self.redo.clear();
            self.version += 1;
            let n = self.frames();
            self.cursor = self.cursor.min(n);
            self.selection = self
                .selection
                .map(|(a, b)| (a.min(n), b.min(n)))
                .filter(|(a, b)| b > a);
        }
    }

    /// Steps back one edit.
    pub fn undo(&mut self) {
        if let Some(a) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.audio, a));
            self.version += 1;
        }
    }

    /// Steps forward one edit.
    pub fn redo(&mut self) {
        if let Some(a) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.audio, a));
            self.version += 1;
        }
    }

    /// Cuts the selection to the clipboard.
    pub fn cut(&mut self) {
        let r = self.range();
        let mut clip = None;
        self.edit(|a| clip = Some(audio_edit::cut(a, r.clone())));
        self.clipboard = clip;
        self.cursor = r.start;
        self.selection = None;
    }

    /// Copies the selection.
    pub fn copy(&mut self) {
        self.clipboard = Some(audio_edit::copy(&self.audio, self.range()));
    }

    /// Pastes at the cursor; the pasted audio becomes the selection.
    pub fn paste(&mut self) {
        let Some(clip) = self.clipboard.clone() else {
            return;
        };
        let at = self.cursor.min(self.frames());
        let n = clip.first().map_or(0, Vec::len);
        self.edit(|a| audio_edit::paste(a, at, &clip));
        self.selection = Some((at, at + n));
    }

    /// Learns the noise print from the selection.
    pub fn learn_noise(&mut self) {
        self.noise = Some(NoiseProfile::learn(&self.mono()));
    }

    /// Reduces noise in the selection with the learnt print.
    pub fn reduce_noise(&mut self) {
        let Some(p) = self.noise.clone() else {
            return;
        };
        let r = self.range();
        let db = self.reduction_db;
        self.edit(|a| {
            for c in a.iter_mut() {
                let out = p.reduce(&c[r.clone()], db);
                c[r.clone()].copy_from_slice(&out);
            }
        });
    }
}

/// Draws the editor's contents; returns what the app must do.
pub fn audio_editor(
    ui: &mut Ui,
    theme: &GloomTheme,
    ed: &mut AudioEditor,
) -> Option<AudioEditorAction> {
    let mut action = None;
    let dim = |s: &str| RichText::new(s).color(theme.text_dim);
    let r = ed.range();
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(!ed.undo.is_empty(), egui::Button::new(tr("Undo")))
            .clicked()
        {
            ed.undo();
        }
        if ui
            .add_enabled(!ed.redo.is_empty(), egui::Button::new(tr("Redo")))
            .clicked()
        {
            ed.redo();
        }
        ui.separator();
        let sel = ed.selection.is_some();
        if ui
            .add_enabled(sel, egui::Button::new(tr("Trim")))
            .on_hover_text(tr("Keep only the selection"))
            .clicked()
        {
            let r = r.clone();
            ed.edit(|a| audio_edit::trim(a, r));
            ed.selection = None;
        }
        if ui.add_enabled(sel, egui::Button::new(tr("Cut"))).clicked() {
            ed.cut();
        }
        if ui.button(tr("Copy")).clicked() {
            ed.copy();
        }
        if ui
            .add_enabled(ed.clipboard.is_some(), egui::Button::new(tr("Paste")))
            .on_hover_text(tr("Insert at the cursor"))
            .clicked()
        {
            ed.paste();
        }
        if ui.button(tr("Silence")).clicked() {
            let r = r.clone();
            ed.edit(|a| audio_edit::silence(a, r));
        }
        ui.separator();
        if ui
            .button(tr("Normalize"))
            .on_hover_text(tr("Peak to -0.1 dBFS"))
            .clicked()
        {
            let r = r.clone();
            ed.edit(|a| audio_edit::normalize(a, r, -0.1));
        }
        if ui.button(tr("Fade in")).clicked() {
            let r = r.clone();
            ed.edit(|a| audio_edit::fade(a, r, true));
        }
        if ui.button(tr("Fade out")).clicked() {
            let r = r.clone();
            ed.edit(|a| audio_edit::fade(a, r, false));
        }
        if ui.button(tr("Reverse")).clicked() {
            let r = r.clone();
            ed.edit(|a| audio_edit::reverse(a, r));
        }
        if ui.button(tr("Remove DC")).clicked() {
            let r = r.clone();
            ed.edit(|a| audio_edit::remove_dc(a, r));
        }
        ui.add(
            egui::DragValue::new(&mut ed.gain_db)
                .range(-48.0..=48.0)
                .speed(0.1)
                .suffix(" dB"),
        );
        if ui.button(tr("Gain")).clicked() {
            let (r, db) = (r.clone(), ed.gain_db);
            ed.edit(|a| audio_edit::gain(a, r, db));
        }
    });
    ui.horizontal_wrapped(|ui| {
        if ui
            .button(tr("Learn noise"))
            .on_hover_text(tr(
                "Select a stretch with only the noise in it, then learn it",
            ))
            .clicked()
        {
            ed.learn_noise();
        }
        ui.add(
            egui::DragValue::new(&mut ed.reduction_db)
                .range(0.0..=60.0)
                .speed(0.2)
                .suffix(" dB"),
        );
        if ui
            .add_enabled(ed.noise.is_some(), egui::Button::new(tr("Reduce noise")))
            .clicked()
        {
            ed.reduce_noise();
        }
        ui.separator();
        if ui
            .button(tr("To MIDI: melody"))
            .on_hover_text(tr("Track the pitch into notes on the selected channel"))
            .clicked()
        {
            action = Some(AudioEditorAction::ToMidi { drums: false });
        }
        if ui
            .button(tr("To MIDI: drums"))
            .on_hover_text(tr("A kick, snare or hat note on every hit"))
            .clicked()
        {
            action = Some(AudioEditorAction::ToMidi { drums: true });
        }
        ui.separator();
        ui.checkbox(&mut ed.spectrogram, tr("Spectrogram"));
        ui.separator();
        if ui
            .add_enabled(ed.changed(), egui::Button::new(tr("Apply")))
            .on_hover_text(tr("Save the edited copy and use it in the clip"))
            .clicked()
        {
            action = Some(AudioEditorAction::Apply);
        }
        if ui.button(tr("Close")).clicked() {
            action = Some(AudioEditorAction::Close);
        }
    });
    let secs = |f: usize| f as f64 / f64::from(ed.rate);
    ui.label(dim(&match ed.selection {
        Some((a, b)) => format!(
            "{:.3} s – {:.3} s ({:.3} s) / {:.3} s",
            secs(a),
            secs(b),
            secs(b - a),
            secs(ed.frames())
        ),
        None => format!("{:.3} s", secs(ed.frames())),
    }));

    let size = vec2(ui.available_width(), ui.available_height().max(160.0));
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let n = ed.frames().max(1);
    if ed.zoom <= 0.0 {
        ed.zoom = n as f64 / f64::from(rect.width().max(1.0));
    }
    // Wheel scrolls, Ctrl+wheel zooms around the pointer.
    if resp.hovered() {
        let (scroll, zoom) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
        if zoom != 1.0 {
            let px = resp.hover_pos().map_or(0.0, |p| p.x - rect.left());
            let at = ed.scroll + f64::from(px) * ed.zoom;
            ed.zoom = (ed.zoom / f64::from(zoom)).clamp(0.05, (n as f64 / 50.0).max(0.05));
            ed.scroll = at - f64::from(px) * ed.zoom;
        } else if scroll.y != 0.0 || scroll.x != 0.0 {
            ed.scroll -= f64::from(scroll.x + scroll.y) * ed.zoom;
        }
    }
    let visible = f64::from(rect.width()) * ed.zoom;
    ed.scroll = ed.scroll.clamp(0.0, (n as f64 - visible).max(0.0));
    let (sc, zm) = (ed.scroll, ed.zoom);
    let frame_at =
        move |x: f32| (sc + f64::from(x - rect.left()) * zm).clamp(0.0, n as f64) as usize;
    let x_of = move |f: usize| rect.left() + ((f as f64 - sc) / zm) as f32;

    if let Some(p) = resp.interact_pointer_pos() {
        let f = frame_at(p.x);
        if resp.drag_started() {
            ed.drag_from = Some(f);
        }
        if let Some(a) = ed.drag_from {
            ed.selection = (f != a).then(|| (a.min(f), a.max(f)));
        }
    }
    if resp.drag_stopped() {
        ed.drag_from = None;
    }
    if resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            ed.cursor = frame_at(p.x);
            ed.selection = None;
        }
    }
    if resp.hovered() || resp.has_focus() {
        let (z, y, x, c, v) = ui.input(|i| {
            let cmd = i.modifiers.command;
            (
                cmd && !i.modifiers.shift && i.key_pressed(egui::Key::Z),
                cmd && (i.key_pressed(egui::Key::Y)
                    || i.modifiers.shift && i.key_pressed(egui::Key::Z)),
                cmd && i.key_pressed(egui::Key::X),
                cmd && i.key_pressed(egui::Key::C),
                cmd && i.key_pressed(egui::Key::V),
            )
        });
        if z {
            ed.undo();
        }
        if y {
            ed.redo();
        }
        if x && ed.selection.is_some() {
            ed.cut();
        }
        if c {
            ed.copy();
        }
        if v {
            ed.paste();
        }
    }

    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, theme.bg_deep);
    let first = ed.scroll as usize;
    let last = ((ed.scroll + visible) as usize).min(n);
    if ed.spectrogram {
        let key = (ed.version, first as u64, last as u64);
        if ed.spec.as_ref().is_none_or(|(k, _)| *k != key) {
            let img = spectrogram(&ed.audio, first..last, 512, 128, theme.accent);
            let tex = ui
                .ctx()
                .load_texture("audio_editor_spec", img, TextureOptions::LINEAR);
            ed.spec = Some((key, tex));
        }
        if let Some((_, tex)) = &ed.spec {
            let w = ((last - first) as f64 / ed.zoom) as f32;
            let r = Rect::from_min_size(rect.min, vec2(w, rect.height()));
            p.image(
                tex.id(),
                r,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
    } else {
        let lanes = ed.audio.len().max(1);
        let lane_h = rect.height() / lanes as f32;
        for (ci, c) in ed.audio.iter().enumerate() {
            let mid = rect.top() + lane_h * (ci as f32 + 0.5);
            p.line_segment(
                [pos2(rect.left(), mid), pos2(rect.right(), mid)],
                Stroke::new(1.0, theme.stroke),
            );
            let cols = rect.width() as usize;
            for col in 0..cols {
                let a = (ed.scroll + col as f64 * ed.zoom) as usize;
                let b = ((ed.scroll + (col + 1) as f64 * ed.zoom) as usize).max(a + 1);
                if a >= c.len() {
                    break;
                }
                let b = b.min(c.len());
                // Long columns are sampled: drawing must not read millions of samples.
                let step = ((b - a) / 256).max(1);
                let (mut lo, mut hi) = (f32::MAX, f32::MIN);
                for &v in c[a..b].iter().step_by(step) {
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
                let x = rect.left() + col as f32 + 0.5;
                let h = lane_h * 0.48;
                p.line_segment(
                    [
                        pos2(x, mid - hi.clamp(-1.0, 1.0) * h),
                        pos2(x, mid - lo.clamp(-1.0, 1.0) * h - 0.5),
                    ],
                    Stroke::new(1.0, theme.accent),
                );
            }
        }
    }
    if let Some((a, b)) = ed.selection {
        let r = Rect::from_x_y_ranges(
            x_of(a).max(rect.left())..=x_of(b).min(rect.right()),
            rect.y_range(),
        );
        p.rect_filled(r, 0.0, theme.text.gamma_multiply(0.15));
    }
    let cx = x_of(ed.cursor);
    if rect.x_range().contains(cx) {
        p.line_segment(
            [pos2(cx, rect.top()), pos2(cx, rect.bottom())],
            Stroke::new(1.0, theme.text),
        );
    }
    action
}

mod spec {
    use super::*;

    /// A spectrogram image of `range`: `cols` columns, `rows` log-spaced bins up to half the
    /// sample rate, in dB from -90 to 0 mapped from dark to `ink`.
    pub fn spectrogram(
        a: &Planar,
        range: std::ops::Range<usize>,
        cols: usize,
        rows: usize,
        ink: Color32,
    ) -> ColorImage {
        const N: usize = 1024;
        let mono: Vec<f32> = range
            .clone()
            .map(|i| {
                a.iter()
                    .map(|c| c.get(i).copied().unwrap_or(0.0))
                    .sum::<f32>()
                    / a.len().max(1) as f32
            })
            .collect();
        let mut px = vec![Color32::BLACK; cols * rows];
        let mut fft = gt_dsp::analysis::Spectrum::new(N);
        let mut db = Vec::new();
        for col in 0..cols {
            let centre = col * mono.len() / cols.max(1);
            let s = centre.saturating_sub(N / 2);
            let mut w: Vec<f32> = mono[s.min(mono.len())..(s + N).min(mono.len())].to_vec();
            w.resize(N, 0.0);
            fft.analyze(&w, &mut db);
            let bins = db.len().max(1);
            for row in 0..rows {
                // Row 0 is the top (highest frequency).
                let t = 1.0 - row as f32 / rows as f32;
                let k = ((bins as f32).powf(t)).clamp(1.0, bins as f32 - 1.0) as usize;
                let v = ((db.get(k).copied().unwrap_or(-120.0) + 90.0) / 90.0).clamp(0.0, 1.0);
                px[row * cols + col] = Color32::from_rgb(
                    (f32::from(ink.r()) * v) as u8,
                    (f32::from(ink.g()) * v) as u8,
                    (f32::from(ink.b()) * v * 0.8 + 20.0 * (1.0 - v)) as u8,
                );
            }
        }
        ColorImage::new([cols, rows], px)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_undo_and_redo_on_the_working_copy() {
        let audio = vec![(0..100).map(|i| i as f32 / 100.0).collect::<Vec<_>>()];
        let mut ed = AudioEditor::new(ClipId(1), "x".into(), 48_000, audio.clone());
        assert!(!ed.changed());
        ed.selection = Some((10, 20));
        ed.cut();
        assert_eq!(ed.frames(), 90);
        ed.cursor = 0;
        ed.paste();
        assert_eq!(ed.selection, Some((0, 10)));
        assert_eq!(ed.audio[0][0], 0.1);
        ed.undo();
        ed.undo();
        assert_eq!(ed.audio, audio);
        ed.redo();
        assert_eq!(ed.frames(), 90);
        // An edit that changes nothing is not an undo step.
        ed.selection = Some((0, 5));
        ed.edit(|a| audio_edit::gain(a, 0..5, 0.0));
        ed.undo();
        assert_eq!(ed.audio, audio);
    }
}
