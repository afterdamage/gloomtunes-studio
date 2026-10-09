//! Standard MIDI Files: export notes as format 1 (one track per channel) and import format 0
//! or 1 files as new channels and a pattern.
//!
//! Export uses the same compiled song the engine plays ([`SongSnapshot`]), so swing, clip
//! repeats and clip cuts come out exactly as heard. Times keep the project's resolution of
//! 960 ticks per quarter note.

use std::path::Path;

use gt_core::{
    Note, PatternId, Project, SigChange, SynthPatch, TempoMap, TempoPoint, Tick, TimeSig,
    TimeSigMap, PPQ,
};
use gt_engine::{NoteKind, SongSnapshot};
use midly::num::{u15, u24, u28, u4, u7};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};

/// What to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmfScope {
    /// The arrangement, as song mode plays it.
    Song,
    /// One pass of a pattern.
    Pattern(PatternId),
}

/// Why a file could not be read.
#[derive(Debug)]
pub enum SmfError {
    /// Reading or writing the file failed.
    Io(std::io::Error),
    /// Not a Standard MIDI File, or a damaged one.
    Parse(String),
    /// The file holds no notes.
    NoNotes,
}

impl std::fmt::Display for SmfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Parse(e) => write!(f, "not a readable MIDI file: {e}"),
            Self::NoNotes => write!(f, "the MIDI file has no notes"),
        }
    }
}

impl std::error::Error for SmfError {}

impl From<std::io::Error> for SmfError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// An event before delta encoding: absolute tick, then the order on the same tick.
type Timed<'a> = (i64, u8, TrackEventKind<'a>);

fn u7c(v: impl Into<i64>) -> u7 {
    u7::new(v.into().clamp(0, 127) as u8)
}

/// Builds the file's bytes.
pub fn to_bytes(project: &Project, scope: SmfScope) -> Vec<u8> {
    let song = match scope {
        SmfScope::Song => SongSnapshot::compile_song(project, |_| None),
        SmfScope::Pattern(id) => SongSnapshot::compile_pattern_once(project, id),
    };
    let names: Vec<Vec<u8>> = project
        .channels
        .iter()
        .map(|c| c.name.as_bytes().to_vec())
        .collect();

    // Track 0: tempo and time signatures.
    let mut conductor: Vec<Timed<'_>> = Vec::new();
    for p in project.tempo.points() {
        let us = (60_000_000.0 / p.bpm)
            .round()
            .clamp(1.0, f64::from(0x00FF_FFFF));
        conductor.push((
            p.at.0,
            0,
            TrackEventKind::Meta(MetaMessage::Tempo(u24::new(us as u32))),
        ));
    }
    for c in project.signatures.changes() {
        let at = project.signatures.bar_start(c.bar);
        let den_pow = c.sig.den.max(1).trailing_zeros() as u8;
        conductor.push((
            at,
            0,
            TrackEventKind::Meta(MetaMessage::TimeSignature(c.sig.num, den_pow, 24, 8)),
        ));
    }
    let mut tracks = vec![conductor];

    // One track per channel that has notes, on MIDI channel (track - 1) mod 16.
    for (slot, name) in names.iter().enumerate() {
        let mut ev: Vec<Timed<'_>> = song
            .events
            .iter()
            .filter(|e| usize::from(e.slot) == slot && e.tick >= 0)
            .map(|e| {
                let message = match e.kind {
                    NoteKind::On { velocity } => MidiMessage::NoteOn {
                        key: u7c(e.key),
                        vel: u7c((velocity * 127.0).round().max(1.0) as i64),
                    },
                    NoteKind::Off => MidiMessage::NoteOff {
                        key: u7c(e.key),
                        vel: u7::new(64),
                    },
                };
                // Note-offs first on a shared tick (as the engine plays them).
                let order = u8::from(matches!(e.kind, NoteKind::On { .. }));
                (e.tick, order, message)
            })
            .map(|(t, o, message)| {
                let channel = u4::new((tracks.len() as u8 - 1) % 16);
                (t, o + 1, TrackEventKind::Midi { channel, message })
            })
            .collect();
        if ev.is_empty() {
            continue;
        }
        ev.push((0, 0, TrackEventKind::Meta(MetaMessage::TrackName(name))));
        tracks.push(ev);
    }

    let mut smf = Smf::new(Header::new(
        Format::Parallel,
        Timing::Metrical(u15::new(PPQ as u16)),
    ));
    for mut t in tracks {
        t.sort_by_key(|e| (e.0, e.1));
        let mut last = 0;
        let mut out: Vec<TrackEvent<'_>> = t
            .into_iter()
            .map(|(at, _, kind)| {
                let delta = (at - last).max(0);
                last = last.max(at);
                TrackEvent {
                    delta: u28::new(delta.min(0x0FFF_FFFF) as u32),
                    kind,
                }
            })
            .collect();
        out.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });
        smf.tracks.push(out);
    }
    let mut bytes = Vec::new();
    // Writing to memory only fails for events midly cannot encode, which we never build.
    let _ = smf.write_std(&mut bytes);
    bytes
}

/// Writes `scope` of `project` to `path` as a format 1 Standard MIDI File.
pub fn write(project: &Project, scope: SmfScope, path: &Path) -> Result<(), SmfError> {
    std::fs::write(path, to_bytes(project, scope))?;
    Ok(())
}

/// One instrument part of an imported file: the notes of one track on one MIDI channel.
#[derive(Debug, Clone, PartialEq)]
pub struct SmfPart {
    /// Track name, or "Track N" / "Channel N".
    pub name: String,
    /// MIDI channel 0 to 15.
    pub channel: u8,
    /// Notes in project ticks from the file's start, sorted.
    pub notes: Vec<Note>,
}

/// The musical content of a MIDI file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SmfImport {
    /// Parts with notes, in file order.
    pub parts: Vec<SmfPart>,
    /// Tempo changes (project ticks, BPM).
    pub tempo: Vec<(i64, f64)>,
    /// Time signature changes (project ticks, signature).
    pub signatures: Vec<(i64, TimeSig)>,
}

/// What [`SmfImport::apply`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    /// The new pattern holding the notes.
    pub pattern: PatternId,
    /// Channels added.
    pub channels: usize,
    /// Parts left out because the channel rack was full.
    pub skipped: usize,
}

/// Reads a format 0 or 1 file with metrical timing.
pub fn parse(bytes: &[u8]) -> Result<SmfImport, SmfError> {
    let smf = Smf::parse(bytes).map_err(|e| SmfError::Parse(e.to_string()))?;
    let ppq = match smf.header.timing {
        Timing::Metrical(t) => i64::from(t.as_int().max(1)),
        Timing::Timecode(..) => {
            return Err(SmfError::Parse(
                "SMPTE (timecode) timing is not supported".to_owned(),
            ))
        }
    };
    let scale = |t: i64| (t * PPQ + ppq / 2).div_euclid(ppq);
    let mut out = SmfImport::default();
    for (ti, track) in smf.tracks.iter().enumerate() {
        let mut name: Option<String> = None;
        // Per channel: held keys (start tick, velocity) and the finished notes.
        let mut held: Vec<[Option<(i64, f32)>; 128]> = vec![[None; 128]; 16];
        let mut notes: Vec<Vec<Note>> = vec![Vec::new(); 16];
        let mut at = 0_i64;
        let end_note = |notes: &mut [Vec<Note>],
                        held: &mut [[Option<(i64, f32)>; 128]],
                        ch: usize,
                        key: usize,
                        at: i64| {
            if let Some((start, velocity)) = held[ch][key].take() {
                let (s, e) = (scale(start), scale(at));
                notes[ch].push(Note {
                    start: s,
                    length: (e - s).max(1),
                    key: key as u8,
                    velocity,
                });
            }
        };
        for ev in track {
            at += i64::from(ev.delta.as_int());
            match ev.kind {
                TrackEventKind::Midi { channel, message } => {
                    let ch = usize::from(channel.as_int());
                    match message {
                        MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                            let k = usize::from(key.as_int());
                            // A key struck again while held ends the earlier note.
                            end_note(&mut notes, &mut held, ch, k, at);
                            held[ch][k] = Some((at, f32::from(vel.as_int()) / 127.0));
                        }
                        MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                            end_note(&mut notes, &mut held, ch, usize::from(key.as_int()), at);
                        }
                        _ => {}
                    }
                }
                TrackEventKind::Meta(MetaMessage::TrackName(n)) if name.is_none() => {
                    let n = String::from_utf8_lossy(n).trim().to_owned();
                    if !n.is_empty() {
                        name = Some(n);
                    }
                }
                TrackEventKind::Meta(MetaMessage::Tempo(us)) => {
                    // Microseconds per quarter cannot hold most tempos exactly; 140 BPM comes
                    // back as 140.00014. A thousandth of a BPM is finer than anyone sets.
                    let us = f64::from(us.as_int().max(1));
                    let bpm = (60_000_000_000.0 / us).round() / 1000.0;
                    out.tempo.push((scale(at), bpm));
                }
                TrackEventKind::Meta(MetaMessage::TimeSignature(num, den_pow, ..)) => {
                    let den = 1_u8.checked_shl(u32::from(den_pow)).unwrap_or(4);
                    out.signatures
                        .push((scale(at), TimeSig::new(num.max(1), den.clamp(2, 16))));
                }
                _ => {}
            }
        }
        // Notes never released end at the track's end.
        for ch in 0..16 {
            for k in 0..128 {
                end_note(&mut notes, &mut held, ch, k, at);
            }
        }
        let used: Vec<usize> = (0..16).filter(|&c| !notes[c].is_empty()).collect();
        for &ch in &used {
            let mut n = std::mem::take(&mut notes[ch]);
            n.sort_by_key(|x| (x.start, x.key));
            let base = name.clone().unwrap_or_else(|| format!("Track {ti}"));
            let name = if used.len() > 1 || smf.header.format == Format::SingleTrack {
                format!("{base} ch {}", ch + 1)
            } else {
                base
            };
            out.parts.push(SmfPart {
                name,
                channel: ch as u8,
                notes: n,
            });
        }
    }
    if out.parts.is_empty() {
        return Err(SmfError::NoNotes);
    }
    out.tempo.sort_by_key(|t| t.0);
    out.signatures.sort_by_key(|s| s.0);
    Ok(out)
}

/// Reads and parses a file.
pub fn read(path: &Path) -> Result<SmfImport, SmfError> {
    parse(&std::fs::read(path)?)
}

impl SmfImport {
    /// Adds one Gloom Synth channel per part and a new pattern `name` holding all the notes,
    /// and selects that pattern. With `use_timing`, the project's tempo and time signatures
    /// become the file's.
    pub fn apply(&self, project: &mut Project, name: &str, use_timing: bool) -> ImportReport {
        let pattern = project.new_pattern();
        project.rename_pattern(pattern, name);
        let mut channels = 0;
        let mut skipped = 0;
        for part in &self.parts {
            let Some(id) = project.add_synth_channel(&part.name, SynthPatch::default()) else {
                skipped += 1;
                continue;
            };
            channels += 1;
            if let Some(p) = project.patterns.iter_mut().find(|p| p.id == pattern) {
                p.notes.insert(id, part.notes.clone());
            }
        }
        project.select_pattern(pattern);
        if use_timing {
            if !self.tempo.is_empty() {
                let mut pts: Vec<TempoPoint> = self
                    .tempo
                    .iter()
                    .map(|&(at, bpm)| TempoPoint { at: Tick(at), bpm })
                    .collect();
                // The map must start at 0.
                if pts[0].at.0 > 0 {
                    pts.insert(
                        0,
                        TempoPoint {
                            at: Tick(0),
                            bpm: pts[0].bpm,
                        },
                    );
                }
                project.tempo = TempoMap::from_points_lossy(&pts);
            }
            if !self.signatures.is_empty() {
                project.signatures = signatures_from_ticks(&self.signatures);
            }
        }
        ImportReport {
            pattern,
            channels,
            skipped,
        }
    }
}

/// Turns signature changes at ticks into changes at bars. A change that falls inside a bar
/// moves to the next bar line.
fn signatures_from_ticks(changes: &[(i64, TimeSig)]) -> TimeSigMap {
    let mut out: Vec<SigChange> = Vec::new();
    for &(at, sig) in changes {
        let map = TimeSigMap::new(&out);
        let bar = if out.is_empty() {
            0
        } else {
            let b = map.bar_of(at);
            if map.bar_start(b) == at {
                b
            } else {
                b + 1
            }
        };
        out.retain(|c| c.bar < bar);
        out.push(SigChange { bar, sig });
    }
    if out.first().is_none_or(|c| c.bar != 0) {
        out.insert(
            0,
            SigChange {
                bar: 0,
                sig: TimeSig::default(),
            },
        );
    }
    TimeSigMap::new(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_pattern_round_trips_through_a_midi_file() {
        let mut p = Project::demo();
        p.swing = 0.0;
        let id = p.current_pattern;
        let bytes = to_bytes(&p, SmfScope::Pattern(id));
        let file = parse(&bytes).unwrap();
        let pat = p.current_pattern();
        let with_notes: Vec<_> = p
            .channels
            .iter()
            .filter(|c| !pat.channel_notes(c.id).is_empty())
            .collect();
        assert_eq!(file.parts.len(), with_notes.len());
        for (part, ch) in file.parts.iter().zip(&with_notes) {
            assert_eq!(part.name, ch.name);
            let want = pat.channel_notes(ch.id);
            assert_eq!(part.notes.len(), want.len());
            for (a, b) in part.notes.iter().zip(want) {
                assert_eq!(
                    (a.start, a.length, a.key),
                    (b.start, b.length.max(1), b.key)
                );
                // Velocity goes through 7 bits.
                assert!((a.velocity - b.velocity).abs() <= 0.5 / 127.0 + 1e-6);
            }
        }
        assert_eq!(file.tempo, vec![(0, 120.0)]);
        assert_eq!(file.signatures, vec![(0, TimeSig::new(4, 4))]);

        // Importing adds a channel per part and a pattern with the same notes.
        let mut q = Project::demo();
        let before = q.channels.len();
        let r = file.apply(&mut q, "Imported", true);
        assert_eq!(r.channels, with_notes.len());
        assert_eq!(q.channels.len(), before + r.channels);
        assert_eq!(q.current_pattern, r.pattern);
        assert_eq!(q.current_pattern().name, "Imported");
        let new = &q.channels[before];
        assert_eq!(
            q.current_pattern().channel_notes(new.id).len(),
            pat.channel_notes(with_notes[0].id).len()
        );
    }

    #[test]
    fn the_song_export_follows_the_arrangement_and_tempo_map() {
        let mut p = Project::demo();
        p.tempo = TempoMap::from_points_lossy(&[
            TempoPoint {
                at: Tick(0),
                bpm: 100.0,
            },
            TempoPoint {
                at: Tick(4 * 3840),
                bpm: 140.0,
            },
        ]);
        let song = SongSnapshot::compile_song(&p, |_| None);
        let ons = song
            .events
            .iter()
            .filter(|e| matches!(e.kind, NoteKind::On { .. }))
            .count();
        let file = parse(&to_bytes(&p, SmfScope::Song)).unwrap();
        assert_eq!(file.parts.iter().map(|x| x.notes.len()).sum::<usize>(), ons);
        assert_eq!(file.tempo, vec![(0, 100.0), (4 * 3840, 140.0)]);
        let first = file.parts[0].notes[0];
        let want = song
            .events
            .iter()
            .find(|e| matches!(e.kind, NoteKind::On { .. }))
            .unwrap();
        assert_eq!(first.start, want.tick);
    }

    #[test]
    fn other_resolutions_and_format_0_files_import() {
        // Format 0 at 96 PPQ: two channels in one track, a note left hanging, a tempo of
        // 90 BPM and 3/4.
        let mut smf = Smf::new(Header::new(
            Format::SingleTrack,
            Timing::Metrical(u15::new(96)),
        ));
        let on = |ch: u8, key: u8| TrackEventKind::Midi {
            channel: u4::new(ch),
            message: MidiMessage::NoteOn {
                key: u7::new(key),
                vel: u7::new(100),
            },
        };
        let off = |ch: u8, key: u8| TrackEventKind::Midi {
            channel: u4::new(ch),
            message: MidiMessage::NoteOn {
                key: u7::new(key),
                vel: u7::new(0),
            },
        };
        let ev = |d: u32, kind| TrackEvent {
            delta: u28::new(d),
            kind,
        };
        smf.tracks.push(vec![
            ev(
                0,
                TrackEventKind::Meta(MetaMessage::Tempo(u24::new(666_667))),
            ),
            ev(
                0,
                TrackEventKind::Meta(MetaMessage::TimeSignature(3, 2, 24, 8)),
            ),
            ev(0, on(0, 60)),
            ev(48, off(0, 60)),
            ev(0, on(9, 36)),
            ev(96, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ]);
        let mut bytes = Vec::new();
        smf.write_std(&mut bytes).unwrap();
        let f = parse(&bytes).unwrap();
        assert_eq!(f.parts.len(), 2);
        assert_eq!(f.parts[0].channel, 0);
        assert_eq!(f.parts[1].channel, 9);
        // 48 ticks at 96 PPQ = an eighth = 480 project ticks.
        assert_eq!(f.parts[0].notes[0].start, 0);
        assert_eq!(f.parts[0].notes[0].length, 480);
        // The hanging note ends at the end of the track.
        assert_eq!(f.parts[1].notes[0].start, 480);
        assert_eq!(f.parts[1].notes[0].length, 960);
        assert!((f.tempo[0].1 - 90.0).abs() < 1e-3);
        assert_eq!(f.signatures, vec![(0, TimeSig::new(3, 4))]);
        let mut p = Project::empty();
        f.apply(&mut p, "x", true);
        assert!((p.tempo.bpm_at(Tick(0)) - 90.0).abs() < 1e-3);
        assert_eq!(p.signatures.sig_at(Tick(0)), TimeSig::new(3, 4));

        assert!(matches!(parse(b"not midi"), Err(SmfError::Parse(_))));
    }

    #[test]
    fn signature_changes_land_on_bar_lines() {
        let m = signatures_from_ticks(&[
            (0, TimeSig::new(4, 4)),
            (2 * 3840, TimeSig::new(3, 4)),
            // Inside a 3/4 bar: moves to the next bar line.
            (2 * 3840 + 1000, TimeSig::new(6, 8)),
        ]);
        let c: Vec<_> = m.changes().map(|c| (c.bar, c.sig)).collect();
        assert_eq!(
            c,
            vec![
                (0, TimeSig::new(4, 4)),
                (2, TimeSig::new(3, 4)),
                (3, TimeSig::new(6, 8))
            ]
        );
    }
}
