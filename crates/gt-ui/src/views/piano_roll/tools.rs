//! The piano roll's Tools menu (Prompt 22). A tool opens a small window on the selection (or
//! every note, when nothing is selected) and previews live: each change of a setting rebuilds
//! the notes from a copy taken when the window opened. Apply keeps the result as one undo
//! step (the app waits for the window to close before committing); Cancel puts the copy back.

use gt_core::note_tools::{
    arpeggiate, chop, flam, flip, generate, glue, legato, randomize, scale_velocity, strum, Arp,
    ArpPattern, Generator, Randomize, Strum,
};

use super::*;
use crate::i18n::tr;

/// A tool from the Tools menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolKind {
    Arpeggiate,
    Strum,
    Chop,
    Flam,
    Glue,
    Legato,
    Flip,
    Randomize,
    ScaleVelocity,
    Generate,
}

impl ToolKind {
    pub(super) const ALL: [ToolKind; 10] = [
        ToolKind::Arpeggiate,
        ToolKind::Strum,
        ToolKind::Chop,
        ToolKind::Flam,
        ToolKind::Glue,
        ToolKind::Legato,
        ToolKind::Flip,
        ToolKind::Randomize,
        ToolKind::ScaleVelocity,
        ToolKind::Generate,
    ];

    pub(super) fn name(self) -> &'static str {
        match self {
            ToolKind::Arpeggiate => "Arpeggiate",
            ToolKind::Strum => "Strum",
            ToolKind::Chop => "Chop",
            ToolKind::Flam => "Flam",
            ToolKind::Glue => "Glue",
            ToolKind::Legato => "Legato",
            ToolKind::Flip => "Flip",
            ToolKind::Randomize => "Randomize",
            ToolKind::ScaleVelocity => "Scale velocities",
            ToolKind::Generate => "Generate chords and bass",
        }
    }

    fn tip(self) -> &'static str {
        match self {
            ToolKind::Arpeggiate => "Play each chord as an arpeggio",
            ToolKind::Strum => "Spread each chord's notes in time",
            ToolKind::Chop => "Split notes into repeats",
            ToolKind::Flam => "Add a grace note before each note",
            ToolKind::Glue => "Join touching notes of the same key",
            ToolKind::Legato => "Make each note last until the next one",
            ToolKind::Flip => "Mirror the selection in time or pitch",
            ToolKind::Randomize => "Random timing, velocity and pitch in the scale",
            ToolKind::ScaleVelocity => "Scale and offset velocities",
            ToolKind::Generate => "Write a chord progression and a bassline in the scale",
        }
    }
}

/// Every tool's settings, kept between uses.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ToolSettings {
    pub(super) arp: Arp,
    pub(super) strum: Strum,
    pub(super) chop: i64,
    pub(super) flam_offset: i64,
    pub(super) flam_velocity: f32,
    pub(super) flip_h: bool,
    pub(super) flip_v: bool,
    pub(super) random: Randomize,
    pub(super) vel_scale: f32,
    pub(super) vel_add: f32,
    pub(super) gen_seed: u64,
    pub(super) gen_bars: u32,
    pub(super) gen_chords: bool,
    pub(super) gen_bass: bool,
    pub(super) gen_octave: u8,
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self {
            arp: Arp {
                pattern: ArpPattern::Up,
                octaves: 1,
                rate: PPQ / 4,
                gate: 0.8,
                seed: 1,
            },
            strum: Strum {
                time: PPQ / 8,
                curve: 0.0,
                down: false,
                velocity: 0.0,
            },
            chop: PPQ / 4,
            flam_offset: PPQ / 32,
            flam_velocity: 0.6,
            flip_h: true,
            flip_v: false,
            random: Randomize {
                seed: 1,
                timing: PPQ / 32,
                velocity: 0.1,
                pitch: 0,
            },
            vel_scale: 1.0,
            vel_add: 0.0,
            gen_seed: 1,
            gen_bars: 4,
            gen_chords: true,
            gen_bass: true,
            gen_octave: 5,
        }
    }
}

/// An open tool window: the notes as they were (note, selected, owner) and the settings
/// last applied.
#[derive(Debug, Clone)]
pub(super) struct ToolDialog {
    kind: ToolKind,
    base: Vec<(Note, bool, u16)>,
    applied: Option<(ToolSettings, ScaleKind, u8)>,
}

/// Opens tool `kind` on the selection (selecting every note when nothing is selected).
pub(super) fn open(st: &mut PianoRollState, notes: &[Note], kind: ToolKind) {
    st.selected.resize(notes.len(), false);
    st.owner.resize(notes.len(), 0);
    if kind != ToolKind::Generate && !st.selected.iter().any(|&s| s) {
        st.selected.iter_mut().for_each(|s| *s = true);
    }
    let base = notes
        .iter()
        .zip(&st.selected)
        .zip(&st.owner)
        .map(|((n, &s), &o)| (*n, s && kind != ToolKind::Generate, o))
        .collect();
    st.drag = None;
    st.dialog = Some(ToolDialog {
        kind,
        base,
        applied: None,
    });
}

/// The notes tool `kind` makes of `input` with settings `t` (`bar_ticks` for the generator).
pub(super) fn apply(
    kind: ToolKind,
    t: &ToolSettings,
    input: &[Note],
    scale: ScaleKind,
    root: u8,
    bar_ticks: i64,
) -> Vec<Note> {
    match kind {
        ToolKind::Arpeggiate => arpeggiate(input, &t.arp),
        ToolKind::Strum => strum(input, &t.strum),
        ToolKind::Chop => chop(input, t.chop),
        ToolKind::Flam => flam(input, t.flam_offset, t.flam_velocity),
        ToolKind::Glue => glue(input),
        ToolKind::Legato => legato(input),
        ToolKind::Flip => flip(input, t.flip_h, t.flip_v),
        ToolKind::Randomize => randomize(input, &t.random, scale.intervals(), root),
        ToolKind::ScaleVelocity => scale_velocity(input, t.vel_scale, t.vel_add),
        ToolKind::Generate => {
            let intervals = if scale == ScaleKind::Off {
                ScaleKind::Major.intervals()
            } else {
                scale.intervals()
            };
            generate(
                &Generator {
                    root,
                    seed: t.gen_seed,
                    bars: t.gen_bars.clamp(1, 64),
                    bar_ticks,
                    chords: t.gen_chords,
                    bass: t.gen_bass,
                    octave: t.gen_octave,
                },
                intervals,
            )
        }
    }
}

/// Rebuilds the working notes from the window's copy with the current settings.
fn rebuild(st: &mut PianoRollState, notes: &mut Vec<Note>, bar_ticks: i64) {
    let Some(d) = st.dialog.take() else {
        return;
    };
    notes.clear();
    st.selected.clear();
    st.owner.clear();
    for &(n, sel, o) in &d.base {
        if !sel {
            push_note(notes, st, n, o, false);
        }
    }
    let owners = d.base.iter().map(|b| b.2).max().unwrap_or(0);
    let outputs: Vec<(Note, u16)> = if d.kind == ToolKind::Generate {
        apply(d.kind, &st.tools, &[], st.scale, st.scale_root, bar_ticks)
            .into_iter()
            .map(|n| (n, 0))
            .collect()
    } else {
        (0..=owners)
            .flat_map(|o| {
                let input: Vec<Note> = d
                    .base
                    .iter()
                    .filter(|b| b.1 && b.2 == o)
                    .map(|b| b.0)
                    .collect();
                apply(
                    d.kind,
                    &st.tools,
                    &input,
                    st.scale,
                    st.scale_root,
                    bar_ticks,
                )
                .into_iter()
                .map(move |n| (n, o))
            })
            .collect()
    };
    for (mut n, o) in outputs {
        n.sanitize();
        n.start = n.start.max(0);
        push_note(notes, st, n, o, true);
    }
    sort_notes(notes, st);
    st.dialog = Some(d);
}

/// Shows the open tool's window. Returns true when the notes changed.
pub(super) fn tool_window(
    ctx: &egui::Context,
    theme: &GloomTheme,
    st: &mut PianoRollState,
    notes: &mut Vec<Note>,
    bar_ticks: i64,
) -> bool {
    let Some(kind) = st.dialog.as_ref().map(|d| d.kind) else {
        return false;
    };
    let mut close: Option<bool> = None;
    let mut t = st.tools.clone();
    egui::Window::new(kind.name())
        .id(egui::Id::new("roll_tool_window"))
        .collapsible(false)
        .resizable(false)
        .default_pos(ctx.content_rect().center() - vec2(140.0, 80.0))
        .show(ctx, |ui| {
            ui.label(RichText::new(kind.tip()).color(theme.text_dim));
            ui.add_space(4.0);
            settings_ui(ui, kind, &mut t, st.scale);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(tr("Apply")).clicked() {
                    close = Some(true);
                }
                if ui.button(tr("Cancel")).clicked() {
                    close = Some(false);
                }
            });
        });
    st.tools = t;
    let mut changed = false;
    let current = (st.tools.clone(), st.scale, st.scale_root);
    let stale = st
        .dialog
        .as_ref()
        .is_some_and(|d| d.applied.as_ref() != Some(&current));
    if stale {
        rebuild(st, notes, bar_ticks);
        if let Some(d) = &mut st.dialog {
            d.applied = Some(current);
        }
        changed = true;
    }
    if let Some(keep) = close {
        changed |= finish(st, notes, keep);
    }
    changed
}

/// Closes the tool window, keeping the preview (`keep`) or putting the notes back. Returns
/// true when the notes changed.
pub(super) fn finish(st: &mut PianoRollState, notes: &mut Vec<Note>, keep: bool) -> bool {
    let Some(d) = st.dialog.take() else {
        return false;
    };
    if keep {
        return false;
    }
    notes.clear();
    st.selected.clear();
    st.owner.clear();
    for (n, s, o) in d.base {
        push_note(notes, st, n, o, s);
    }
    true
}

/// A choice of note divisions for a length in ticks.
fn division(ui: &mut Ui, id: &str, label: &str, ticks: &mut i64) {
    ui.horizontal(|ui| {
        ui.label(label);
        let current = Snap::ALL
            .iter()
            .find(|s| s.ticks() == *ticks)
            .map_or_else(|| format!("{ticks} ticks"), |s| s.label());
        egui::ComboBox::from_id_salt(id)
            .width(64.0)
            .selected_text(current)
            .show_ui(ui, |ui| {
                for s in Snap::ALL.iter().filter(|s| **s != Snap::Off) {
                    ui.selectable_value(ticks, s.ticks(), s.label());
                }
            });
    });
}

fn percent(ui: &mut Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>) {
    let mut x = *v * 100.0;
    let r = (*range.start() * 100.0)..=(*range.end() * 100.0);
    if ui
        .add(
            egui::DragValue::new(&mut x)
                .range(r)
                .prefix(format!("{label} "))
                .suffix(" %")
                .speed(0.5),
        )
        .changed()
    {
        *v = x / 100.0;
    }
}

fn settings_ui(ui: &mut Ui, kind: ToolKind, t: &mut ToolSettings, scale: ScaleKind) {
    match kind {
        ToolKind::Arpeggiate => {
            ui.horizontal(|ui| {
                for p in ArpPattern::ALL {
                    ui.selectable_value(&mut t.arp.pattern, p, p.name());
                }
            });
            ui.add(
                egui::DragValue::new(&mut t.arp.octaves)
                    .range(1..=4)
                    .prefix(tr("Octaves ")),
            );
            division(ui, "arp_rate", "Rate", &mut t.arp.rate);
            percent(ui, "Gate", &mut t.arp.gate, 0.05..=1.0);
            if t.arp.pattern == ArpPattern::Random {
                ui.add(egui::DragValue::new(&mut t.arp.seed).prefix(tr("Seed ")));
            }
        }
        ToolKind::Strum => {
            ui.add(
                egui::DragValue::new(&mut t.strum.time)
                    .range(0..=PPQ * 2)
                    .prefix(tr("Time "))
                    .suffix(tr(" ticks")),
            );
            percent(ui, "Curve", &mut t.strum.curve, -1.0..=1.0);
            percent(ui, "Velocity", &mut t.strum.velocity, -1.0..=1.0);
            ui.checkbox(&mut t.strum.down, tr("Strum down (top key first)"));
        }
        ToolKind::Chop => division(ui, "chop_step", "Pieces", &mut t.chop),
        ToolKind::Flam => {
            ui.add(
                egui::DragValue::new(&mut t.flam_offset)
                    .range(1..=PPQ)
                    .prefix(tr("Before "))
                    .suffix(tr(" ticks")),
            );
            percent(ui, "Grace velocity", &mut t.flam_velocity, 0.0..=1.0);
        }
        ToolKind::Glue | ToolKind::Legato => {}
        ToolKind::Flip => {
            ui.checkbox(
                &mut t.flip_h,
                tr("In time (around the middle of the selection)"),
            );
            ui.checkbox(
                &mut t.flip_v,
                tr("In pitch (around the middle of its keys)"),
            );
        }
        ToolKind::Randomize => {
            ui.add(egui::DragValue::new(&mut t.random.seed).prefix(tr("Seed ")));
            ui.add(
                egui::DragValue::new(&mut t.random.timing)
                    .range(0..=PPQ)
                    .prefix(tr("Timing ± "))
                    .suffix(tr(" ticks")),
            );
            percent(ui, "Velocity ±", &mut t.random.velocity, 0.0..=1.0);
            ui.add(
                egui::DragValue::new(&mut t.random.pitch)
                    .range(0..=12)
                    .prefix(tr("Pitch ± "))
                    .suffix(tr(" st")),
            );
            if t.random.pitch > 0 {
                let note = if scale == ScaleKind::Off {
                    "Any key (no scale chosen)".to_owned()
                } else {
                    format!("Keys stay in {}", scale.name())
                };
                ui.label(note);
            }
        }
        ToolKind::ScaleVelocity => {
            percent(ui, "Scale", &mut t.vel_scale, 0.0..=2.0);
            percent(ui, "Add", &mut t.vel_add, -1.0..=1.0);
        }
        ToolKind::Generate => {
            ui.add(egui::DragValue::new(&mut t.gen_seed).prefix(tr("Style seed ")));
            ui.add(
                egui::DragValue::new(&mut t.gen_bars)
                    .range(1..=64)
                    .prefix(tr("Bars ")),
            );
            ui.add(
                egui::DragValue::new(&mut t.gen_octave)
                    .range(3..=7)
                    .prefix(tr("Octave ")),
            );
            ui.checkbox(&mut t.gen_chords, tr("Chords"));
            ui.checkbox(&mut t.gen_bass, tr("Bassline"));
            let scale = if scale == ScaleKind::Off {
                "C major (choose a scale in the toolbar)".to_owned()
            } else {
                format!("In the toolbar's {}", scale.name())
            };
            ui.label(scale);
        }
    }
}

/// The Tools menu and the chord stamp in the toolbar.
pub(super) fn tools_menu(ui: &mut Ui, st: &mut PianoRollState) {
    ui.menu_button(tr("Tools"), |ui| {
        for k in ToolKind::ALL {
            if k == ToolKind::Generate {
                ui.separator();
            }
            if ui.button(k.name()).on_hover_text(k.tip()).clicked() {
                st.pending_tool = Some(k);
                ui.close();
            }
        }
    })
    .response
    .on_hover_text(tr(
        "Tools for the selection (or all notes), with a live preview",
    ));
    let label = st.chord.map_or("No chord", |c| c.name());
    super::fit(ui, 96.0);
    egui::ComboBox::from_id_salt("roll_chord")
        .width(96.0)
        .selected_text(label)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut st.chord, None, tr("No chord"));
            for c in gt_core::note_tools::ChordKind::ALL {
                ui.selectable_value(&mut st.chord, Some(c), c.name());
            }
        })
        .response
        .on_hover_text(tr("Chord stamp: drawing a note places this chord on it"));
    if st.chord.is_some() {
        ui.add(
            egui::DragValue::new(&mut st.chord_inversion)
                .range(0..=3)
                .prefix(tr("Inv ")),
        )
        .on_hover_text(tr("Inversion: each moves the lowest note up an octave"));
    }
}
