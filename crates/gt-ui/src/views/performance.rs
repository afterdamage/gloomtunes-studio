//! Performance mode (Prompt 31): the playlist as a grid of launchable clips. A row per visible
//! track, a cell per pattern or audio clip of it in timeline order, a stop cell at the end.
//! Click a cell to launch it on the next launch point; keys 1 to 9 launch that column on every
//! track (a scene); the app also launches from MIDI notes on channel 16.

use egui::{pos2, vec2, Color32, RichText, Sense, Stroke, StrokeKind, Ui};
use gt_core::performance::{grid_row, LaunchQuant, Performance};
use gt_core::{ClipId, ClipKind, Project, TrackId};

use crate::i18n::tr;
use crate::theme::GloomTheme;

use super::playlist::PlaylistAction;

const CELL_W: f32 = 112.0;
const NAME_W: f32 = 120.0;

/// What the grid shows of the performance in progress.
pub struct PerformanceView<'a> {
    /// The launches so far.
    pub perf: &'a Performance,
    /// Playhead in ticks.
    pub position: i64,
    /// Whether the performance is being recorded.
    pub recording: bool,
}

/// The cells of the visible tracks, top to bottom.
pub fn grid(project: &Project) -> Vec<(TrackId, Vec<ClipId>)> {
    let pl = &project.playlist;
    pl.visible_rows()
        .into_iter()
        .map(|i| pl.tracks[i].id)
        .filter(|&t| !pl.track(t).is_some_and(|t| t.folder))
        .map(|t| (t, grid_row(pl, t)))
        .collect()
}

fn clip_name(project: &Project, id: ClipId) -> String {
    match project.playlist.clip(id).map(|c| &c.kind) {
        Some(ClipKind::Pattern(p)) => project
            .pattern(*p)
            .map_or_else(String::new, |p| p.name.clone()),
        Some(ClipKind::Audio { source, .. }) => source.display_name(),
        _ => String::new(),
    }
}

/// Draws the grid; launches and settings come back as actions.
pub fn performance_grid(
    ui: &mut Ui,
    theme: &GloomTheme,
    project: &Project,
    view: &PerformanceView<'_>,
    actions: &mut Vec<PlaylistAction>,
) {
    let dim = |s: &str| RichText::new(s).color(theme.text_dim);
    let rows = grid(project);
    ui.horizontal(|ui| {
        ui.label(dim(tr("Launch on")));
        let mut q = view.perf.quant;
        egui::ComboBox::from_id_salt("perf_quant")
            .width(60.0)
            .selected_text(tr(LaunchQuant::NAMES[q.index()]))
            .show_ui(ui, |ui| {
                for (k, opt) in LaunchQuant::ALL.into_iter().enumerate() {
                    ui.selectable_value(&mut q, opt, tr(LaunchQuant::NAMES[k]));
                }
            });
        if q != view.perf.quant {
            actions.push(PlaylistAction::SetLaunchQuant(q));
        }
        ui.separator();
        let rec = ui
            .add(egui::Button::selectable(
                view.recording,
                tr("Record performance"),
            ))
            .on_hover_text(tr(
                "When playback stops, the launches become a new arrangement",
            ));
        if rec.clicked() {
            actions.push(PlaylistAction::RecordPerformance(!view.recording));
        }
        if ui.button(tr("Stop all clips")).clicked() {
            for (t, _) in &rows {
                actions.push(PlaylistAction::Launch {
                    track: *t,
                    clip: None,
                });
            }
        }
    });
    ui.add_space(4.0);

    let scenes = rows.iter().map(|r| r.1.len()).max().unwrap_or(0);
    egui::ScrollArea::both().show(ui, |ui| {
        // Scene buttons: a column on every track at once.
        ui.horizontal(|ui| {
            ui.add_space(NAME_W);
            for k in 0..scenes {
                let label = format!("{} {}", tr("Scene"), k + 1);
                if ui
                    .add_sized(vec2(CELL_W - 4.0, 18.0), egui::Button::new(label))
                    .on_hover_text(tr("Launch this column on every track (keys 1 to 9)"))
                    .clicked()
                {
                    actions.push(PlaylistAction::LaunchScene(k));
                }
            }
        });
        for (track, cells) in &rows {
            let name = project
                .playlist
                .track(*track)
                .map_or_else(String::new, |t| t.name.clone());
            let playing = view.perf.playing(*track, view.position);
            let queued = view.perf.queued(*track, view.position);
            ui.horizontal(|ui| {
                ui.add_sized(vec2(NAME_W - 8.0, 28.0), egui::Label::new(name).truncate());
                for &c in cells {
                    let text = clip_name(project, c);
                    let (r, resp) =
                        ui.allocate_exact_size(vec2(CELL_W - 4.0, 28.0), Sense::click());
                    let is_playing = playing == Some(c);
                    let is_queued = queued.is_some_and(|q| q.clip == Some(c));
                    let base = project.playlist.track(*track).map_or(theme.bg_widget, |t| {
                        Color32::from_rgb(t.color[0], t.color[1], t.color[2])
                    });
                    let fill = if is_playing {
                        base
                    } else {
                        base.gamma_multiply(0.35)
                    };
                    let p = ui.painter();
                    p.rect_filled(r, 3.0, fill);
                    if is_queued || resp.hovered() {
                        p.rect_stroke(
                            r,
                            3.0,
                            Stroke::new(if is_queued { 2.0 } else { 1.0 }, theme.accent),
                            StrokeKind::Inside,
                        );
                    }
                    // A play mark on the playing cell.
                    if is_playing {
                        let c0 = pos2(r.left() + 8.0, r.center().y);
                        p.add(egui::Shape::convex_polygon(
                            vec![
                                c0 + vec2(-3.0, -5.0),
                                c0 + vec2(5.0, 0.0),
                                c0 + vec2(-3.0, 5.0),
                            ],
                            theme.text,
                            Stroke::NONE,
                        ));
                    }
                    p.text(
                        pos2(r.left() + 16.0, r.center().y),
                        egui::Align2::LEFT_CENTER,
                        text,
                        egui::FontId::proportional(theme.font_size - 1.0),
                        theme.text,
                    );
                    if resp.clicked() {
                        actions.push(PlaylistAction::Launch {
                            track: *track,
                            clip: Some(c),
                        });
                    }
                }
                // Missing cells keep the stop buttons in one column.
                let step = CELL_W - 4.0 + ui.spacing().item_spacing.x;
                ui.add_space((scenes - cells.len()) as f32 * step);
                let stop = ui
                    .add_sized(vec2(28.0, 28.0), egui::Button::new("■"))
                    .on_hover_text(tr("Stop this track's clip"));
                if stop.clicked() {
                    actions.push(PlaylistAction::Launch {
                        track: *track,
                        clip: None,
                    });
                }
            });
        }
        if rows.is_empty() {
            ui.label(dim(tr(
                "No clips: place patterns or audio on tracks to launch them",
            )));
        }
    });

    if ui.ui_contains_pointer() && !ui.ctx().text_edit_focused() {
        let keys = [
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
            egui::Key::Num6,
            egui::Key::Num7,
            egui::Key::Num8,
            egui::Key::Num9,
        ];
        for (k, key) in keys.into_iter().enumerate() {
            if ui.input(|i| i.key_pressed(key) && i.modifiers.is_none()) {
                actions.push(PlaylistAction::LaunchScene(k));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_has_a_row_per_track_with_clips_in_time_order() {
        let p = Project::demo();
        let g = grid(&p);
        assert!(!g.is_empty());
        for (t, cells) in &g {
            let starts: Vec<i64> = cells
                .iter()
                .map(|&c| p.playlist.clip(c).unwrap().start)
                .collect();
            assert!(starts.windows(2).all(|w| w[0] <= w[1]));
            assert!(cells
                .iter()
                .all(|&c| p.playlist.clip(c).unwrap().track == *t));
        }
    }
}
