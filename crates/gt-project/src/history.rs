//! Undo/redo as a command stack.
//!
//! The UI edits the document directly. At the end of each gesture (mouse released, text field
//! left, a key command done) the app calls [`History::commit`], which compares the document with
//! the last committed state and records the difference as one undoable step:
//!
//! - when only notes changed, one [`Edit::Notes`] per changed (pattern, channel) list, holding
//!   that list before and after (a step of a 10,000-note pattern costs only the lists it
//!   touched);
//! - anything else (channels, pattern list, swing, the mixer) is recorded as [`Edit::Whole`], a before/after
//!   copy of the document. Those edits are rare and small.
//!
//! Which pattern is selected is view state and is never undone. A drag that moves notes for a
//! second is one undo step because it is committed once, on release.

use gt_core::{ChannelId, Note, PatternId, Project};

/// Undo steps kept by default.
pub const DEFAULT_LIMIT: usize = 200;

/// One reversible change.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// The notes of one channel in one pattern.
    Notes {
        /// Pattern.
        pattern: PatternId,
        /// Channel.
        channel: ChannelId,
        /// Notes before.
        before: Vec<Note>,
        /// Notes after.
        after: Vec<Note>,
    },
    /// The whole document.
    Whole {
        /// Document before.
        before: Box<Project>,
        /// Document after.
        after: Box<Project>,
    },
}

/// One undo step: the edits of one gesture.
#[derive(Debug, Clone, PartialEq)]
pub struct Transaction {
    /// Short description for menus and tooltips.
    pub label: String,
    /// Edits, applied in order (reverted in reverse order).
    pub edits: Vec<Edit>,
}

/// The undo and redo stacks plus the last committed document.
#[derive(Debug, Clone)]
pub struct History {
    base: Project,
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    limit: usize,
}

impl History {
    /// Starts a history whose first state is `project`.
    pub fn new(project: &Project) -> Self {
        Self {
            base: project.clone(),
            undo: Vec::new(),
            redo: Vec::new(),
            limit: DEFAULT_LIMIT,
        }
    }

    /// Records the change from the last committed state to `project` as one undo step. Returns
    /// false (and records nothing) if nothing undoable changed. Clears the redo stack.
    pub fn commit(&mut self, project: &Project, label: &str) -> bool {
        let edits = diff(&self.base, project);
        if edits.is_empty() {
            // Selection changes etc.: keep the base current without an undo step.
            self.base.current_pattern = project.current_pattern;
            return false;
        }
        self.undo.push(Transaction {
            label: label.to_owned(),
            edits,
        });
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.base = project.clone();
        true
    }

    /// Reverts the last step. Uncommitted changes in `project` are discarded first, as if the
    /// gesture had not happened. Returns the step's label.
    pub fn undo(&mut self, project: &mut Project) -> Option<String> {
        let t = self.undo.pop()?;
        let selected = project.current_pattern;
        *project = self.base.clone();
        for e in t.edits.iter().rev() {
            apply(project, e, Side::Before);
        }
        restore_selection(project, selected);
        self.base = project.clone();
        let label = t.label.clone();
        self.redo.push(t);
        Some(label)
    }

    /// Re-applies the last undone step. Returns its label.
    pub fn redo(&mut self, project: &mut Project) -> Option<String> {
        let t = self.redo.pop()?;
        let selected = project.current_pattern;
        *project = self.base.clone();
        for e in &t.edits {
            apply(project, e, Side::After);
        }
        restore_selection(project, selected);
        self.base = project.clone();
        let label = t.label.clone();
        self.undo.push(t);
        Some(label)
    }

    /// Label of the step `undo` would revert.
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|t| t.label.as_str())
    }

    /// Label of the step `redo` would re-apply.
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|t| t.label.as_str())
    }

    /// Number of undo steps available.
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }
}

/// Keeps the user's selected pattern if it still exists, else selects the first one.
fn restore_selection(p: &mut Project, selected: PatternId) {
    p.select_pattern(selected);
    if !p.patterns.iter().any(|x| x.id == p.current_pattern) {
        p.current_pattern = p.patterns[0].id;
    }
}

#[derive(Clone, Copy)]
enum Side {
    Before,
    After,
}

fn apply(p: &mut Project, e: &Edit, side: Side) {
    match e {
        Edit::Notes {
            pattern,
            channel,
            before,
            after,
        } => {
            let notes = match side {
                Side::Before => before,
                Side::After => after,
            };
            if let Some(pat) = p.patterns.iter_mut().find(|x| x.id == *pattern) {
                if notes.is_empty() {
                    pat.notes.remove(channel);
                } else {
                    pat.notes.insert(*channel, notes.clone());
                }
            }
        }
        Edit::Whole { before, after } => {
            let doc = match side {
                Side::Before => before,
                Side::After => after,
            };
            *p = (**doc).clone();
        }
    }
}

/// The edits that turn `a` into `b`, ignoring the selected pattern.
fn diff(a: &Project, b: &Project) -> Vec<Edit> {
    let same_shape = a.channels == b.channels
        && a.swing == b.swing
        && a.mixer == b.mixer
        && a.patterns.len() == b.patterns.len()
        && a.patterns
            .iter()
            .zip(&b.patterns)
            .all(|(x, y)| x.id == y.id && x.name == y.name && x.steps == y.steps);
    if !same_shape {
        let mut before = a.clone();
        let mut after = b.clone();
        before.current_pattern = b.current_pattern;
        after.current_pattern = b.current_pattern;
        return vec![Edit::Whole {
            before: Box::new(before),
            after: Box::new(after),
        }];
    }
    let mut edits = Vec::new();
    for (x, y) in a.patterns.iter().zip(&b.patterns) {
        if x.notes == y.notes {
            continue;
        }
        let keys: std::collections::BTreeSet<_> = x.notes.keys().chain(y.notes.keys()).collect();
        for &ch in keys {
            let before = x.channel_notes(ch);
            let after = y.channel_notes(ch);
            if before != after {
                edits.push(Edit::Notes {
                    pattern: x.id,
                    channel: ch,
                    before: before.to_vec(),
                    after: after.to_vec(),
                });
            }
        }
    }
    edits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_edits_undo_and_redo() {
        let mut p = Project::demo();
        let mut h = History::new(&p);
        let kick = p.channels[0].id;
        p.current_pattern_mut().toggle_step(kick, 1);
        p.current_pattern_mut().toggle_step(kick, 2);
        assert!(h.commit(&p, "Add steps"));
        let after = p.clone();
        assert_eq!(h.undo(&mut p).as_deref(), Some("Add steps"));
        assert_eq!(p, Project::demo());
        assert_eq!(h.redo(&mut p).as_deref(), Some("Add steps"));
        assert_eq!(p, after);
        assert!(h.redo(&mut p).is_none());
    }

    #[test]
    fn only_touched_note_lists_are_stored() {
        let mut p = Project::demo();
        let mut h = History::new(&p);
        let hat = p.channels[2].id;
        p.current_pattern_mut().set_step_velocity(hat, 2, 0.1);
        h.commit(&p, "Velocity");
        let t = &h.undo[0];
        assert_eq!(t.edits.len(), 1);
        assert!(matches!(&t.edits[0], Edit::Notes { channel, .. } if *channel == hat));
    }

    #[test]
    fn structural_edits_are_whole_and_selection_is_not_undone() {
        let mut p = Project::demo();
        let mut h = History::new(&p);
        let channels = p.channels.len();
        let first = p.current_pattern;
        let second = p.new_pattern();
        p.select_pattern(second);
        h.commit(&p, "New pattern");
        p.add_channel("Extra", None);
        h.commit(&p, "Add channel");
        // Selecting another pattern alone is not an undo step.
        p.select_pattern(first);
        assert!(!h.commit(&p, "Select"));
        h.undo(&mut p);
        assert_eq!(p.channels.len(), channels);
        assert_eq!(p.current_pattern, first, "selection kept");
        h.redo(&mut p);
        p.select_pattern(second);
        h.undo(&mut p);
        h.undo(&mut p);
        assert_eq!(p.patterns.len(), 1);
        assert_eq!(p.current_pattern, first, "deleted selection falls back");
        assert!(h.undo(&mut p).is_none());
    }

    #[test]
    fn undo_discards_an_uncommitted_change() {
        let mut p = Project::demo();
        let mut h = History::new(&p);
        p.swing = 0.5;
        h.commit(&p, "Swing");
        p.swing = 0.9; // not committed
        h.undo(&mut p);
        assert_eq!(p.swing, 0.0);
    }

    #[test]
    fn new_commit_clears_redo_and_limit_applies() {
        let mut p = Project::empty();
        let mut h = History::new(&p);
        h.limit = 3;
        for k in 0..5 {
            p.swing = k as f32 * 0.1 + 0.1;
            h.commit(&p, "Swing");
        }
        assert_eq!(h.undo_len(), 3);
        h.undo(&mut p);
        assert!(h.redo_label().is_some());
        p.swing = 0.0;
        h.commit(&p, "Swing");
        assert!(h.redo_label().is_none());
    }

    #[test]
    fn mixer_changes_are_undoable() {
        let mut p = Project::demo();
        let mut h = History::new(&p);
        p.mixer.strips[3].volume = 0.25;
        p.mixer.strips[2].slots[4] = Some(gt_core::EffectSlot::new(gt_core::EffectKind::Chorus));
        assert!(h.commit(&p, "Mixer"));
        h.undo(&mut p);
        assert_eq!(p.mixer, Project::demo().mixer);
        h.redo(&mut p);
        assert_eq!(p.mixer.strips[3].volume, 0.25);
    }
}
