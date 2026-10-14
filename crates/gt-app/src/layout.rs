//! Window layout (Prompt 25, ARCHITECTURE.md D133): which main view is docked in the main
//! window, which float in windows of their own (native windows that can sit on another
//! monitor), where those windows are, and whether the browser is shown. Saved in the
//! settings file and restored at start.

use serde::{Deserialize, Serialize};

/// The four main views.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MainView {
    Playlist,
    Rack,
    PianoRoll,
    Mixer,
}

impl MainView {
    pub const ALL: [MainView; 4] = [
        MainView::Playlist,
        MainView::Rack,
        MainView::PianoRoll,
        MainView::Mixer,
    ];

    pub fn key(self) -> &'static str {
        match self {
            MainView::Playlist => "playlist",
            MainView::Rack => "rack",
            MainView::PianoRoll => "piano_roll",
            MainView::Mixer => "mixer",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            MainView::Playlist => gt_ui::tr("Playlist"),
            MainView::Rack => gt_ui::tr("Channel rack"),
            MainView::PianoRoll => gt_ui::tr("Piano roll"),
            MainView::Mixer => gt_ui::tr("Mixer"),
        }
    }

    /// The id of the view's own window.
    pub fn viewport_id(self) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("gt-view", self.key()))
    }
}

/// A view in a window of its own.
#[derive(Debug, Clone, PartialEq)]
pub struct Floating {
    pub view: MainView,
    /// Where the window opened (kept fixed, so egui never moves it while it is dragged).
    pub opened_at: Option<egui::Rect>,
    /// Where it is now: outer position and inner size, in points.
    pub rect: Option<egui::Rect>,
}

/// The whole layout.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// The selected tab of the main window.
    pub view: MainView,
    /// The browser panel is shown.
    pub browser: bool,
    /// Views in windows of their own, in the order they were floated.
    pub floating: Vec<Floating>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            view: MainView::Rack,
            browser: true,
            floating: Vec::new(),
        }
    }
}

impl Layout {
    pub fn is_floating(&self, v: MainView) -> bool {
        self.floating.iter().any(|f| f.view == v)
    }

    /// The view the main window shows: the selected tab, or the first docked view when the
    /// selected one floats. `None` when every view floats.
    pub fn docked_view(&self) -> Option<MainView> {
        if !self.is_floating(self.view) {
            return Some(self.view);
        }
        MainView::ALL.into_iter().find(|&v| !self.is_floating(v))
    }

    /// True when `v` is on screen (docked and selected, or floating).
    pub fn visible(&self, v: MainView) -> bool {
        self.is_floating(v) || self.docked_view() == Some(v)
    }

    /// Moves `v` into a window of its own at `at` (or where the system puts it).
    pub fn float(&mut self, v: MainView, at: Option<egui::Rect>) {
        if !self.is_floating(v) {
            self.floating.push(Floating {
                view: v,
                opened_at: at,
                rect: at,
            });
        }
    }

    /// Puts `v` back into the main window and selects it.
    pub fn dock(&mut self, v: MainView) {
        self.floating.retain(|f| f.view != v);
        self.view = v;
    }

    /// For the settings file.
    pub fn to_prefs(&self) -> LayoutPrefs {
        LayoutPrefs {
            view: self.view.key().to_owned(),
            browser: self.browser,
            windows: self
                .floating
                .iter()
                .map(|f| {
                    let r = f.rect.or(f.opened_at);
                    WindowPrefs {
                        view: f.view.key().to_owned(),
                        rect: r.map(|r| [r.min.x, r.min.y, r.width(), r.height()]),
                    }
                })
                .collect(),
        }
    }

    /// From the settings file: unknown views and repeats are dropped, and windows too small
    /// to use open at the system's default size.
    pub fn from_prefs(p: &LayoutPrefs) -> Self {
        let mut l = Self {
            view: MainView::from_key(&p.view).unwrap_or(MainView::Rack),
            browser: p.browser,
            floating: Vec::new(),
        };
        for w in &p.windows {
            let Some(v) = MainView::from_key(&w.view) else {
                continue;
            };
            let rect = w
                .rect
                .filter(|r| r.iter().all(|x| x.is_finite()) && r[2] >= 200.0 && r[3] >= 120.0)
                .map(|[x, y, w, h]| egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h)));
            l.float(v, rect);
        }
        l
    }
}

/// [`Layout`] in the settings file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutPrefs {
    pub view: String,
    pub browser: bool,
    pub windows: Vec<WindowPrefs>,
}

impl Default for LayoutPrefs {
    fn default() -> Self {
        Layout::default().to_prefs()
    }
}

/// One floating window: its view and its outer position and inner size (x, y, w, h).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowPrefs {
    pub view: String,
    #[serde(default)]
    pub rect: Option<[f32; 4]>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floating_and_docking_choose_what_the_main_window_shows() {
        let mut l = Layout::default();
        assert_eq!(l.docked_view(), Some(MainView::Rack));
        l.float(MainView::Rack, None);
        // The selected tab floats: the main window shows the first docked view.
        assert_eq!(l.docked_view(), Some(MainView::Playlist));
        assert!(l.visible(MainView::Rack) && l.visible(MainView::Playlist));
        assert!(!l.visible(MainView::Mixer));
        for v in [MainView::Playlist, MainView::PianoRoll, MainView::Mixer] {
            l.float(v, None);
        }
        l.float(MainView::Mixer, None);
        assert_eq!(l.floating.len(), 4, "floated once");
        assert_eq!(l.docked_view(), None);
        l.dock(MainView::Mixer);
        assert_eq!(l.docked_view(), Some(MainView::Mixer));
        assert_eq!(l.view, MainView::Mixer);
    }

    #[test]
    fn layout_saves_and_loads() {
        let mut l = Layout {
            view: MainView::PianoRoll,
            browser: false,
            floating: Vec::new(),
        };
        let r = egui::Rect::from_min_size(egui::pos2(1930.0, 40.0), egui::vec2(900.0, 600.0));
        l.float(MainView::Mixer, Some(r));
        l.float(MainView::Playlist, None);
        // Moved to the second monitor after opening.
        l.floating[1].rect = Some(egui::Rect::from_min_size(
            egui::pos2(-1600.0, 100.0),
            egui::vec2(1200.0, 700.0),
        ));
        let json = serde_json::to_string(&l.to_prefs()).unwrap();
        let back = Layout::from_prefs(&serde_json::from_str(&json).unwrap());
        assert_eq!(back.view, l.view);
        assert!(!back.browser);
        let rects: Vec<_> = back
            .floating
            .iter()
            .map(|f| (f.view, f.opened_at))
            .collect();
        assert_eq!(
            rects,
            vec![
                (MainView::Mixer, Some(r)),
                (MainView::Playlist, l.floating[1].rect)
            ]
        );
        // Damaged entries are dropped or opened at the default place.
        let bad = LayoutPrefs {
            view: "nonsense".into(),
            browser: true,
            windows: vec![
                WindowPrefs {
                    view: "teapot".into(),
                    rect: None,
                },
                WindowPrefs {
                    view: "rack".into(),
                    rect: Some([0.0, 0.0, 5.0, f32::NAN]),
                },
                WindowPrefs {
                    view: "rack".into(),
                    rect: None,
                },
            ],
        };
        let l = Layout::from_prefs(&bad);
        assert_eq!(l.view, MainView::Rack);
        assert_eq!(l.floating.len(), 1);
        assert_eq!(l.floating[0].opened_at, None);
        assert_eq!(
            Layout::from_prefs(&LayoutPrefs::default()),
            Layout::default()
        );
    }
}
