//! Notes found in audio put into the song (Prompt 32, D146): the audio editor's "To MIDI".

use crate::playlist::ClipKind;
use crate::project::{ChannelId, Note, PatternId, Project};

/// A note found in audio, in seconds from where the audio starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimedNote {
    /// Start.
    pub start_s: f64,
    /// Length.
    pub length_s: f64,
    /// MIDI key.
    pub key: u8,
    /// 0 to 1.
    pub velocity: f32,
}

impl Project {
    /// Puts `notes` (seconds from timeline tick `at`, through the song's tempo map) into a new
    /// pattern named `name` for `channel`, and a clip of it at `at` on a new track. Returns the
    /// pattern, or `None` when there are no notes or no such channel.
    pub fn notes_from_audio(
        &mut self,
        name: &str,
        channel: ChannelId,
        at: i64,
        notes: &[TimedNote],
    ) -> Option<PatternId> {
        if notes.is_empty() || !self.channels.iter().any(|c| c.id == channel) {
            return None;
        }
        let tempo = self.song_tempo();
        let at = at.max(0);
        let s0 = tempo.tick_to_seconds(at as f64);
        let tick = |s: f64| (tempo.seconds_to_tick(s0 + s.max(0.0)).round() as i64 - at).max(0);
        let mut out: Vec<Note> = notes
            .iter()
            .map(|n| {
                let start = tick(n.start_s);
                let end = tick(n.start_s + n.length_s).max(start + 1);
                Note::new(
                    start,
                    end - start,
                    n.key.min(127),
                    n.velocity.clamp(0.0, 1.0),
                )
            })
            .collect();
        out.sort_by_key(|n| (n.start, n.key));
        let id = self.new_pattern();
        let unique = self.unique_pattern_name(name);
        let pat = self.patterns.iter_mut().find(|p| p.id == id)?;
        pat.name = unique.clone();
        pat.notes.insert(channel, out);
        let len = self.pattern(id)?.length_ticks();
        let track = self.playlist.add_track();
        if let Some(t) = self.playlist.track_mut(track) {
            t.name = unique;
        }
        self.playlist
            .add_clip(track, at, len, ClipKind::Pattern(id));
        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::TempoMap;
    use crate::PPQ;

    #[test]
    fn notes_land_on_the_ticks_their_times_play_at() {
        let mut p = Project::demo();
        p.tempo = TempoMap::constant(120.0);
        let ch = p.channels[0].id;
        let tracks = p.playlist.tracks.len();
        let notes = [
            TimedNote {
                start_s: 0.0,
                length_s: 0.25,
                key: 60,
                velocity: 0.8,
            },
            TimedNote {
                start_s: 0.5,
                length_s: 0.5,
                key: 64,
                velocity: 0.4,
            },
        ];
        let id = p.notes_from_audio("Vox", ch, 4 * PPQ, &notes).unwrap();
        let pat = p.pattern(id).unwrap();
        assert!(pat.name.starts_with("Vox"));
        let got = pat.channel_notes(ch);
        // 120 BPM: 0.5 s is one beat.
        assert_eq!((got[0].start, got[0].length, got[0].key), (0, PPQ / 2, 60));
        assert_eq!((got[1].start, got[1].length, got[1].key), (PPQ, PPQ, 64));
        assert_eq!(p.playlist.tracks.len(), tracks + 1);
        let clip = p.playlist.clips.last().unwrap();
        assert_eq!(clip.start, 4 * PPQ);
        assert!(matches!(clip.kind, ClipKind::Pattern(x) if x == id));
        assert!(p.notes_from_audio("x", ch, 0, &[]).is_none());
    }
}
