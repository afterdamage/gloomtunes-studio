//! Multi-sample key maps for the sampler (Prompt 23, ARCHITECTURE.md D127): which sample a
//! note plays, at which root, for which keys and velocities. Built off the audio thread from
//! the document and the loaded samples, handed to a channel as an `Arc`, and read-only there.

use std::sync::Arc;

use gt_core::{Adsr, DrumKit, SampleData, SampleSource, SamplerSettings};

/// Most separate outputs (mixer inserts) of one channel's pads.
pub const MAX_AUX: usize = 8;

/// One sample of a key map.
#[derive(Debug, Clone)]
pub struct Zone {
    /// The audio.
    pub data: Arc<SampleData>,
    /// Key at which it plays at its recorded pitch.
    pub root: u8,
    /// Lowest key.
    pub key_lo: u8,
    /// Highest key.
    pub key_hi: u8,
    /// Lowest velocity.
    pub vel_lo: f32,
    /// Highest velocity.
    pub vel_hi: f32,
    /// Fine tune in semitones.
    pub tune: f32,
    /// Linear gain.
    pub gain: f32,
    /// Pan, -1 to 1 (added to the note's).
    pub pan: f32,
    /// Its own amplitude envelope (drum pads); `None` uses the channel's.
    pub env: Option<Adsr>,
    /// Choke group (0: none): starting it cuts the sounding voices of the group.
    pub choke: u8,
    /// Separate output: 1 + index into [`ZoneMap::routes`] (0: the channel's own).
    pub aux: u8,
    /// Part of the sample played (fractions; a slice), or `None` for the channel's
    /// start and end.
    pub region: Option<(f32, f32)>,
}

impl Zone {
    /// A zone over every key and velocity.
    pub fn new(data: Arc<SampleData>, root: u8) -> Self {
        Self {
            data,
            root,
            key_lo: 0,
            key_hi: 127,
            vel_lo: 0.0,
            vel_hi: 1.0,
            tune: 0.0,
            gain: 1.0,
            pan: 0.0,
            env: None,
            choke: 0,
            aux: 0,
            region: None,
        }
    }

    /// True when the zone answers `key` at `velocity`.
    #[inline]
    pub fn matches(&self, key: u8, velocity: f32) -> bool {
        key >= self.key_lo
            && key <= self.key_hi
            && velocity >= self.vel_lo
            && (velocity <= self.vel_hi || self.vel_hi >= 1.0)
    }
}

/// The zones of a sampler channel, in document order.
#[derive(Debug, Clone, Default)]
pub struct ZoneMap {
    /// Zones; several may answer the same note (velocity layers overlap only at their edges,
    /// round-robin takes share a range).
    pub zones: Vec<Zone>,
    /// Mixer inserts of the separate outputs zones use (see [`Zone::aux`]).
    pub routes: Vec<u8>,
}

impl ZoneMap {
    /// The key map of a sampler: its zones that are loaded (`sample` gives the data of a
    /// source), or, without zones, its one sample over the channel's key range. `None` when
    /// nothing is loaded.
    pub fn build(
        s: &SamplerSettings,
        sample: impl Fn(&SampleSource) -> Option<Arc<SampleData>>,
    ) -> Option<Self> {
        let zones: Vec<Zone> = if s.zones.is_empty() {
            let data = sample(s.sample.as_ref()?)?;
            vec![Zone {
                key_lo: s.key_lo,
                key_hi: s.key_hi.max(s.key_lo),
                ..Zone::new(data, s.root_key)
            }]
        } else {
            s.zones
                .iter()
                .filter_map(|z| {
                    Some(Zone {
                        data: sample(&z.sample)?,
                        root: z.root,
                        key_lo: z.key_lo,
                        key_hi: z.key_hi,
                        vel_lo: z.vel_lo,
                        vel_hi: z.vel_hi,
                        tune: z.tune,
                        gain: z.gain,
                        region: (z.start > 0.0 || z.end < 1.0).then_some((z.start, z.end)),
                        ..Zone::new(Arc::new(SampleData::default()), 0)
                    })
                })
                .collect()
        };
        (!zones.is_empty()).then_some(Self {
            zones,
            routes: Vec::new(),
        })
    }

    /// The key map of a drum kit: each loaded layer of each pad, on the pad's key, with the
    /// pad's tuning, gain, pan, envelope, choke group and output. `None` when nothing is
    /// loaded.
    pub fn kit(
        kit: &DrumKit,
        sample: impl Fn(&SampleSource) -> Option<Arc<SampleData>>,
    ) -> Option<Self> {
        let mut map = Self::default();
        for (key, pad) in kit.pads() {
            let aux = match pad.output.and_then(|o| u8::try_from(o).ok()) {
                None => 0,
                Some(o) => match map.routes.iter().position(|&r| r == o) {
                    Some(i) => i as u8 + 1,
                    None if map.routes.len() < MAX_AUX => {
                        map.routes.push(o);
                        map.routes.len() as u8
                    }
                    None => 0,
                },
            };
            for l in &pad.layers {
                let Some(data) = sample(&l.sample) else {
                    continue;
                };
                map.zones.push(Zone {
                    data,
                    root: key,
                    key_lo: key,
                    key_hi: key,
                    vel_lo: l.vel_lo,
                    vel_hi: l.vel_hi,
                    tune: pad.tune,
                    gain: pad.gain,
                    pan: pad.pan,
                    env: Some(pad.adsr),
                    choke: pad.choke,
                    aux,
                    region: None,
                });
            }
        }
        (!map.zones.is_empty()).then_some(map)
    }

    /// The zones that answer `key` at `velocity`: every one (layered), or with `round_robin`
    /// the `turn`-th of them (wrapping). Writes indices into `out` and returns how many.
    pub fn pick(
        &self,
        key: u8,
        velocity: f32,
        round_robin: bool,
        turn: u32,
        out: &mut [u16],
    ) -> usize {
        let mut n = 0;
        let matching = || {
            self.zones
                .iter()
                .enumerate()
                .filter(|(_, z)| z.matches(key, velocity))
                .map(|(i, _)| i as u16)
        };
        if round_robin {
            let count = matching().count();
            if count > 0 && !out.is_empty() {
                let k = turn as usize % count;
                if let Some(i) = matching().nth(k) {
                    out[0] = i;
                    n = 1;
                }
            }
        } else {
            for i in matching() {
                if n == out.len() {
                    break;
                }
                out[n] = i;
                n += 1;
            }
        }
        n
    }
}

/// What a sampler channel plays: its single sample (the time-stretch render `fitted` when
/// there is one) or its key map. Off the audio thread.
pub fn sampler_sources(
    s: &SamplerSettings,
    sample: impl Fn(&SampleSource) -> Option<Arc<SampleData>>,
    fitted: Option<Arc<SampleData>>,
) -> (Option<Arc<SampleData>>, Option<Arc<ZoneMap>>) {
    if s.zones.is_empty() {
        let single = fitted.or_else(|| s.sample.as_ref().and_then(&sample));
        (single, None)
    } else {
        (None, ZoneMap::build(s, sample).map(Arc::new))
    }
}

/// What channel `ch` plays from samples: a sampler's ([`sampler_sources`]) or a drum kit's
/// key map. `fitted` is a sampler's time-stretch render, if ready.
pub fn channel_sources(
    ch: &gt_core::Channel,
    sample: impl Fn(&SampleSource) -> Option<Arc<SampleData>>,
    fitted: Option<Arc<SampleData>>,
) -> (Option<Arc<SampleData>>, Option<Arc<ZoneMap>>) {
    match &ch.instrument {
        gt_core::Instrument::Sampler(s) => sampler_sources(s, sample, fitted),
        gt_core::Instrument::Drums(k) => (None, ZoneMap::kit(k, sample).map(Arc::new)),
        _ => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gt_core::KeyZone;

    fn data() -> Arc<SampleData> {
        Arc::new(SampleData::mono(48_000, vec![0.0; 16]))
    }

    fn map() -> ZoneMap {
        let z = |lo, hi, vlo, vhi| Zone {
            key_lo: lo,
            key_hi: hi,
            vel_lo: vlo,
            vel_hi: vhi,
            ..Zone::new(data(), 60)
        };
        ZoneMap {
            zones: vec![
                z(0, 59, 0.0, 0.5),
                z(0, 59, 0.5, 1.0),
                z(60, 127, 0.0, 1.0),
                z(60, 127, 0.0, 1.0),
                z(60, 127, 0.0, 1.0),
            ],
            routes: Vec::new(),
        }
    }

    #[test]
    fn key_zones_and_velocity_layers_select() {
        let m = map();
        let mut out = [0u16; 8];
        let pick = |k, v, out: &mut [u16]| {
            let n = m.pick(k, v, false, 0, out);
            out[..n].to_vec()
        };
        assert_eq!(pick(40, 0.2, &mut out), [0]);
        assert_eq!(pick(40, 0.9, &mut out), [1]);
        // At the boundary both layers answer.
        assert_eq!(pick(40, 0.5, &mut out), [0, 1]);
        // Without round robin, the three takes layer.
        assert_eq!(pick(72, 0.5, &mut out), [2, 3, 4]);
    }

    #[test]
    fn round_robin_takes_turns_in_order() {
        let m = map();
        let mut out = [0u16; 8];
        let order: Vec<u16> = (0..7)
            .map(|turn| {
                assert_eq!(m.pick(72, 0.5, true, turn, &mut out), 1);
                out[0]
            })
            .collect();
        assert_eq!(order, [2, 3, 4, 2, 3, 4, 2]);
    }

    #[test]
    fn build_uses_the_zones_or_the_single_sample() {
        let mut s = SamplerSettings {
            sample: Some(SampleSource::File("a.wav".into())),
            key_lo: 36,
            key_hi: 48,
            root_key: 40,
            ..SamplerSettings::default()
        };
        let m = ZoneMap::build(&s, |_| Some(data())).unwrap();
        assert_eq!(m.zones.len(), 1);
        assert_eq!(
            (m.zones[0].root, m.zones[0].key_lo, m.zones[0].key_hi),
            (40, 36, 48)
        );
        assert!(ZoneMap::build(&s, |_| None).is_none());
        s.zones = vec![
            KeyZone::new(SampleSource::File("b.wav".into()), 50),
            KeyZone::new(SampleSource::File("c.wav".into()), 70),
        ];
        // Only loaded zones take part.
        let m = ZoneMap::build(&s, |src| {
            (src == &SampleSource::File("c.wav".into())).then(data)
        })
        .unwrap();
        assert_eq!(m.zones.len(), 1);
        assert_eq!(m.zones[0].root, 70);
        assert_eq!(m.zones[0].region, None);
        // Slices keep their part of the sample.
        s.zones = gt_core::sampler::slice_zones(&SampleSource::File("l.wav".into()), &[0, 8], 16);
        let m = ZoneMap::build(&s, |_| Some(data())).unwrap();
        assert_eq!(m.zones[1].region, Some((0.5, 1.0)));
        assert_eq!(m.zones[0].region, Some((0.0, 0.5)));
    }
}
