//! Several arrangements in one project (Prompt 30, D143).
//!
//! An arrangement is a playlist (tracks, clips, markers) with its own tempo map and time
//! signatures; patterns, channels and the mixer are shared. The current arrangement lives in
//! [`Project::playlist`], [`Project::tempo`] and [`Project::signatures`], so everything that
//! edits, plays or exports the song keeps working on those fields. The others are stored in
//! [`Project::arrangements`]; the current one's entry there keeps only its name (its content is
//! moved out while it is current). Switching swaps the content in and out.

use crate::playlist::{ClipId, ClipKind, Playlist};
use crate::project::Project;
use crate::time::{TempoMap, Tick, TimeSigMap};

/// Most arrangements a project holds.
pub const MAX_ARRANGEMENTS: usize = 64;

/// One arrangement.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Arrangement {
    /// Name shown in the switcher.
    pub name: String,
    /// Tracks, clips and markers (empty while this arrangement is current).
    pub playlist: Playlist,
    /// Tempo over the timeline (default while current).
    pub tempo: TempoMap,
    /// Time signatures over the timeline (default while current).
    pub signatures: TimeSigMap,
}

impl Arrangement {
    /// An entry that holds only a name (the current arrangement's, or a placeholder).
    pub fn named(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            playlist: Playlist::empty(),
            tempo: TempoMap::default(),
            signatures: TimeSigMap::default(),
        }
    }
}

impl Project {
    /// Number of arrangements (at least 1).
    pub fn arrangement_count(&self) -> usize {
        self.arrangements.len()
    }

    /// Names of the arrangements, in order.
    pub fn arrangement_names(&self) -> impl Iterator<Item = &str> {
        self.arrangements.iter().map(|a| a.name.as_str())
    }

    /// Every arrangement's playlist: the current one first, then the stored ones.
    pub fn playlists(&self) -> impl Iterator<Item = &Playlist> {
        let cur = self.current_arrangement;
        std::iter::once(&self.playlist).chain(
            self.arrangements
                .iter()
                .enumerate()
                .filter(move |(i, _)| *i != cur)
                .map(|(_, a)| &a.playlist),
        )
    }

    /// Every arrangement's playlist, mutably (the current one first).
    pub fn playlists_mut(&mut self) -> impl Iterator<Item = &mut Playlist> {
        let cur = self.current_arrangement;
        std::iter::once(&mut self.playlist).chain(
            self.arrangements
                .iter_mut()
                .enumerate()
                .filter(move |(i, _)| *i != cur)
                .map(|(_, a)| &mut a.playlist),
        )
    }

    /// Arrangement `i` with its content, cloned (the current one from the live fields).
    pub fn arrangement(&self, i: usize) -> Option<Arrangement> {
        let a = self.arrangements.get(i)?;
        Some(if i == self.current_arrangement {
            Arrangement {
                name: a.name.clone(),
                playlist: self.playlist.clone(),
                tempo: self.tempo.clone(),
                signatures: self.signatures.clone(),
            }
        } else {
            a.clone()
        })
    }

    /// Makes arrangement `i` current. False when it does not exist or already is current.
    pub fn switch_arrangement(&mut self, i: usize) -> bool {
        if i >= self.arrangements.len() || i == self.current_arrangement {
            return false;
        }
        let cur = self.current_arrangement;
        let out = &mut self.arrangements[cur];
        out.playlist = std::mem::replace(&mut self.playlist, Playlist::empty());
        out.tempo = std::mem::take(&mut self.tempo);
        out.signatures = std::mem::take(&mut self.signatures);
        let into = &mut self.arrangements[i];
        self.playlist = std::mem::replace(&mut into.playlist, Playlist::empty());
        self.tempo = std::mem::take(&mut into.tempo);
        self.signatures = std::mem::take(&mut into.signatures);
        self.current_arrangement = i;
        true
    }

    /// Adds an empty arrangement after the last one, with the current tempo at its start and
    /// the current time signature, and makes it current. Returns its index, or `None` when
    /// there are [`MAX_ARRANGEMENTS`].
    pub fn add_arrangement(&mut self) -> Option<usize> {
        if self.arrangements.len() >= MAX_ARRANGEMENTS {
            return None;
        }
        let name = self.unique_arrangement_name("Arrangement");
        let tempo = TempoMap::constant(self.tempo.bpm_at(Tick(0)));
        let signatures = TimeSigMap::constant(self.signatures.sig_of_bar(0));
        self.arrangements.push(Arrangement {
            name,
            playlist: Playlist::new(),
            tempo,
            signatures,
        });
        let i = self.arrangements.len() - 1;
        self.switch_arrangement(i);
        Some(i)
    }

    /// Copies arrangement `i` (clips, markers and tempo; patterns stay shared) right after
    /// it and makes the copy current. Returns its index.
    pub fn duplicate_arrangement(&mut self, i: usize) -> Option<usize> {
        if self.arrangements.len() >= MAX_ARRANGEMENTS {
            return None;
        }
        let mut copy = self.arrangement(i)?;
        copy.name = self.unique_arrangement_name(&format!("{} copy", copy.name));
        let at = i + 1;
        self.arrangements.insert(at, copy);
        if self.current_arrangement >= at {
            self.current_arrangement += 1;
        }
        // The copy's content is in its entry; switching moves it into the live fields.
        self.switch_arrangement(at);
        Some(at)
    }

    /// Renames arrangement `i`. Empty names are ignored.
    pub fn rename_arrangement(&mut self, i: usize, name: &str) {
        let name = name.trim();
        if let Some(a) = self.arrangements.get_mut(i).filter(|_| !name.is_empty()) {
            a.name = name.chars().take(64).collect();
        }
    }

    /// Deletes arrangement `i`; the last one stays. A deleted current arrangement makes its
    /// neighbour current. False when nothing was deleted.
    pub fn remove_arrangement(&mut self, i: usize) -> bool {
        if self.arrangements.len() < 2 || i >= self.arrangements.len() {
            return false;
        }
        if i == self.current_arrangement {
            let next = if i + 1 < self.arrangements.len() {
                i + 1
            } else {
                i - 1
            };
            self.switch_arrangement(next);
        }
        self.arrangements.remove(i);
        if self.current_arrangement > i {
            self.current_arrangement -= 1;
        }
        true
    }

    /// Moves arrangement `from` to position `to` in the switcher.
    pub fn move_arrangement(&mut self, from: usize, to: usize) {
        let n = self.arrangements.len();
        if from >= n || to >= n || from == to {
            return;
        }
        let a = self.arrangements.remove(from);
        self.arrangements.insert(to, a);
        let c = self.current_arrangement;
        self.current_arrangement = if c == from {
            to
        } else if from < c && c <= to {
            c - 1
        } else if to <= c && c < from {
            c + 1
        } else {
            c
        };
    }

    /// `base`, or `base 2`, `base 3`… so no arrangement has that name yet.
    pub fn unique_arrangement_name(&self, base: &str) -> String {
        let free = |s: &str| self.arrangements.iter().all(|a| a.name != s);
        if free(base) && base != "Arrangement" {
            return base.to_owned();
        }
        (if base == "Arrangement" { 1 } else { 2 }..)
            .map(|k| format!("{base} {k}"))
            .find(|s| free(s))
            .unwrap_or_default()
    }

    /// Gives clips `ids` sources of their own: each pattern among their sources is copied once
    /// (the chosen clips of that pattern play the copy; other clips keep the original), and
    /// the chosen linked automation clips leave their link (several chosen clips of one link
    /// share a new one). Audio clips are unaffected. Returns the new patterns.
    pub fn make_unique(&mut self, ids: &[ClipId]) -> Vec<crate::PatternId> {
        let mut made = Vec::new();
        let mut patterns: Vec<crate::PatternId> = Vec::new();
        let mut links: Vec<u32> = Vec::new();
        for c in self.playlist.clips.iter().filter(|c| ids.contains(&c.id)) {
            match c.kind {
                ClipKind::Pattern(p) if !patterns.contains(&p) => patterns.push(p),
                ClipKind::Automation(_) if c.link != 0 && !links.contains(&c.link) => {
                    links.push(c.link);
                }
                _ => {}
            }
        }
        for p in patterns {
            // Already unique: every clip of the pattern is chosen (in every arrangement).
            let all_chosen = self
                .playlists()
                .flat_map(|pl| &pl.clips)
                .filter(|c| c.kind == ClipKind::Pattern(p))
                .all(|c| ids.contains(&c.id) && self.playlist.clip(c.id).is_some());
            if all_chosen {
                continue;
            }
            let Some(copy) = self.clone_pattern(p) else {
                continue;
            };
            made.push(copy);
            for c in &mut self.playlist.clips {
                if ids.contains(&c.id) && c.kind == ClipKind::Pattern(p) {
                    c.kind = ClipKind::Pattern(copy);
                }
            }
        }
        for l in links {
            let chosen = |c: &crate::Clip| ids.contains(&c.id) && c.link == l;
            let n = self.playlist.clips.iter().filter(|c| chosen(c)).count();
            let total = self.playlist.clips.iter().filter(|c| c.link == l).count();
            if n == total {
                continue;
            }
            let fresh = if n > 1 { self.playlist.new_link() } else { 0 };
            for c in &mut self.playlist.clips {
                if chosen(c) {
                    c.link = fresh;
                }
            }
        }
        made
    }

    /// Puts the arrangement list in order after loading or an edit: at least one, the
    /// current index in range, names set, stored playlists sanitized like the current one.
    pub(crate) fn sanitize_arrangements(
        &mut self,
        pattern_exists: impl Fn(crate::PatternId) -> bool,
    ) {
        self.arrangements.truncate(MAX_ARRANGEMENTS);
        if self.arrangements.is_empty() {
            self.arrangements.push(Arrangement::named("Arrangement 1"));
        }
        if self.current_arrangement >= self.arrangements.len() {
            self.current_arrangement = 0;
        }
        for i in 0..self.arrangements.len() {
            if self.arrangements[i].name.trim().is_empty() {
                self.arrangements[i].name = format!("Arrangement {}", i + 1);
            }
        }
        let cur = self.current_arrangement;
        for (i, a) in self.arrangements.iter_mut().enumerate() {
            if i == cur {
                // Only the name is kept for the current arrangement.
                a.playlist = Playlist::empty();
                a.tempo = TempoMap::default();
                a.signatures = TimeSigMap::default();
            } else {
                a.playlist.sanitize(&pattern_exists);
                let next = a.playlist.id_counter();
                a.playlist.set_id_counter(next);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playlist::ClipKind;

    #[test]
    fn arrangements_switch_duplicate_rename_and_remove() {
        let mut p = Project::demo();
        let song = p.playlist.clone();
        let bpm = p.tempo.bpm_at(Tick(0));
        assert_eq!(p.arrangement_count(), 1);
        // A new arrangement is empty, at the same tempo, and current.
        let b = p.add_arrangement().unwrap();
        assert_eq!((b, p.current_arrangement), (1, 1));
        assert!(p.playlist.clips.is_empty());
        assert_eq!(p.tempo.bpm_at(Tick(0)), bpm);
        p.tempo = TempoMap::constant(90.0);
        let pid = p.patterns[0].id;
        let t = p.playlist.tracks[0].id;
        p.playlist.add_clip(t, 0, 3840, ClipKind::Pattern(pid));
        p.playlist.add_marker(0, "Verse");
        // Switching back restores the first one exactly, and its own tempo.
        assert!(p.switch_arrangement(0));
        assert_eq!(p.playlist, song);
        assert_eq!(p.tempo.bpm_at(Tick(0)), bpm);
        assert_eq!(p.playlists().count(), 2);
        assert_eq!(p.arrangement(1).unwrap().tempo.bpm_at(Tick(0)), 90.0);
        // A duplicate is an independent copy placed after its original.
        let c = p.duplicate_arrangement(1).unwrap();
        assert_eq!((c, p.current_arrangement), (2, 2));
        assert_eq!(p.arrangements[2].name, "Arrangement 2 copy");
        assert_eq!(p.playlist.markers[0].name, "Verse");
        p.playlist.markers[0].name = "Chorus".into();
        assert_eq!(p.arrangement(1).unwrap().playlist.markers[0].name, "Verse");
        p.rename_arrangement(2, "  Radio edit ");
        p.rename_arrangement(2, " ");
        assert_eq!(p.arrangements[2].name, "Radio edit");
        // Removing the current one makes a neighbour current; the last one stays.
        assert!(p.remove_arrangement(2));
        assert_eq!((p.arrangement_count(), p.current_arrangement), (2, 1));
        assert_eq!(p.playlist.markers[0].name, "Verse");
        assert!(p.remove_arrangement(0));
        assert!(!p.remove_arrangement(0));
        assert_eq!(p.current_arrangement, 0);
        assert_eq!(p.tempo.bpm_at(Tick(0)), 90.0);
    }

    #[test]
    fn moving_an_arrangement_keeps_the_current_one() {
        let mut p = Project::empty();
        p.add_arrangement();
        p.add_arrangement();
        p.switch_arrangement(1);
        let names: Vec<String> = p.arrangement_names().map(str::to_owned).collect();
        p.move_arrangement(0, 2);
        assert_eq!(p.arrangements[p.current_arrangement].name, names[1]);
        p.move_arrangement(2, 0);
        assert_eq!(
            p.arrangement_names().collect::<Vec<_>>(),
            names.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert_eq!(p.current_arrangement, 1);
    }

    #[test]
    fn made_unique_clips_edit_independently() {
        let mut p = Project::demo();
        let main = p.playlist.clips[1].clone();
        let ClipKind::Pattern(pid) = main.kind else {
            panic!("pattern clip");
        };
        let same = p.playlist.clips_of_source(main.id);
        assert!(same.len() >= 2, "the demo plays its main pattern twice");
        let made = p.make_unique(&[main.id]);
        assert_eq!(made.len(), 1);
        let copy = made[0];
        // Notes added to the copy do not reach the original and the other way round.
        let ch = p.channels[0].id;
        p.patterns
            .iter_mut()
            .find(|x| x.id == copy)
            .unwrap()
            .notes
            .remove(&ch);
        assert_ne!(
            p.pattern(copy).unwrap().channel_notes(ch),
            p.pattern(pid).unwrap().channel_notes(ch)
        );
        assert_eq!(
            p.playlist.clip(main.id).unwrap().kind,
            ClipKind::Pattern(copy)
        );
        assert_eq!(p.playlist.clips_of_source(main.id), vec![main.id]);
        assert!(same
            .iter()
            .filter(|&&c| c != main.id)
            .all(|&c| p.playlist.clip(c).unwrap().kind == ClipKind::Pattern(pid)));
        // A clip that already plays its own pattern stays as it is.
        assert!(p.make_unique(&[main.id]).is_empty());

        // Duplicated automation clips are linked; an edit to one reaches the copy until it is
        // made unique.
        let auto = p
            .playlist
            .clips
            .iter()
            .find(|c| matches!(c.kind, ClipKind::Automation(_)))
            .unwrap()
            .id;
        let dup = p.playlist.duplicate_clips(&[auto], 3840)[0];
        assert_eq!(p.playlist.clips_of_source(auto), vec![auto, dup]);
        let edit = |p: &mut Project, id, v| {
            let before = p.playlist.clone();
            if let ClipKind::Automation(a) = &mut p.playlist.clip_mut(id).unwrap().kind {
                a.points[0].value = v;
            }
            p.playlist.sync_links(&before)
        };
        let first = |p: &Project, id| match &p.playlist.clip(id).unwrap().kind {
            ClipKind::Automation(a) => a.points[0].value,
            _ => unreachable!(),
        };
        assert_eq!(edit(&mut p, auto, 0.25), 1);
        assert_eq!(first(&p, dup), 0.25);
        assert!(p.make_unique(&[dup]).is_empty());
        assert_eq!(edit(&mut p, dup, 0.75), 0);
        assert_eq!((first(&p, auto), first(&p, dup)), (0.25, 0.75));
        assert_eq!(p.playlist.clips_of_source(dup), vec![dup]);
    }

    #[test]
    fn replacing_a_source_keeps_position_and_length() {
        let mut p = Project::demo();
        let c = p.playlist.clips[0].clone();
        let other = p.patterns.last().unwrap().id;
        assert_eq!(p.playlist.replace_source(&[c.id], Some(other), None), 1);
        let n = p.playlist.clip(c.id).unwrap();
        assert_eq!(
            (n.start, n.length, n.offset, &n.kind),
            (c.start, c.length, c.offset, &ClipKind::Pattern(other))
        );
        // Automation clips have no source to replace.
        let auto = p
            .playlist
            .clips
            .iter()
            .find(|c| matches!(c.kind, ClipKind::Automation(_)))
            .unwrap()
            .id;
        assert_eq!(p.playlist.replace_source(&[auto], Some(other), None), 0);
    }
}
