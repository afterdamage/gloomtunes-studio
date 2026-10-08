//! Proves that `AudioProcessor::process` never allocates or frees memory, including while it
//! applies commands, plays the metronome through loop wraps and swaps tempo maps.
//!
//! `assert_no_alloc` installs a global allocator for this test binary only; any allocation or
//! deallocation inside `assert_no_alloc(|| ...)` aborts the test process.

use assert_no_alloc::{assert_no_alloc, AllocDisabler};
use std::sync::Arc;

use gt_core::{
    ChannelParam, ClipKind, EffectKind, EffectSlot, LfoRate, LfoShape, ModSourceKind, ParamId,
    Project, SampleData, SampleSource, SigChange, StripParam, SynthParam, TempoMap, TempoPoint,
    Tick, TimeSig, TimeSigMap, MAX_CHANNELS,
};
use gt_engine::{
    create, create_effect, ChannelParams, EngineCommand, EngineConfig, LoopRegion, MixerParams,
    ModPlan, SongSnapshot,
};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn process_does_not_allocate() {
    for (sr, ch, block) in [(44_100, 2, 441), (48_000, 1, 64), (96_000, 6, 1024)] {
        let (mut h, mut p) = create(EngineConfig {
            sample_rate: sr,
            out_channels: ch,
        });
        let mut buf = vec![0.0_f32; block * ch];
        let mut run = |p: &mut gt_engine::AudioProcessor, n: usize| {
            for _ in 0..n {
                assert_no_alloc(|| p.process(&mut buf));
            }
        };
        run(&mut p, 10);

        h.send(EngineCommand::SetTestTone(true)).unwrap();
        h.send(EngineCommand::SetSignatures(Box::new(TimeSigMap::new(&[
            SigChange {
                bar: 0,
                sig: TimeSig::new(7, 8),
            },
            SigChange {
                bar: 1,
                sig: TimeSig::new(3, 4),
            },
        ]))))
        .unwrap();
        h.send(EngineCommand::SetLoop(LoopRegion {
            start: Tick(960),
            end: Tick(960 * 3),
            enabled: true,
        }))
        .unwrap();
        h.send(EngineCommand::Play).unwrap();
        run(&mut p, 200);
        assert_eq!(h.collect_garbage(), 1, "the old signatures");

        // Tempo swaps: the Box is allocated here, the old one is freed by collect_garbage().
        for bpm in [90.0, 300.0, 61.5] {
            h.send(EngineCommand::SetTempoMap(Box::new(TempoMap::constant(
                bpm,
            ))))
            .unwrap();
            run(&mut p, 50);
            assert_eq!(h.collect_garbage(), 1);
        }

        // Samples, channel settings, a pattern, auditions and the preview voice. Every payload
        // is allocated here and comes back through the garbage queue.
        let kick = Arc::new(SampleData::mono(sr, gt_dsp::drums::kick(sr as f32)));
        let hat = Arc::new(SampleData::mono(sr, gt_dsp::drums::hat(sr as f32)));
        let mut project = Project::demo();
        project.swing = 0.3;
        for slot in 0..MAX_CHANNELS as u16 {
            let sample = if slot % 2 == 0 { &kick } else { &hat };
            h.send(EngineCommand::SetChannelSample {
                slot,
                sample: Some(Arc::clone(sample)),
            })
            .unwrap();
            for _ in 0..10 {
                h.send(EngineCommand::SetChannelParams {
                    slot,
                    params: Box::new(ChannelParams {
                        gain: 0.5,
                        pan: -0.5,
                        looped: slot % 3 == 0,
                        ..ChannelParams::default()
                    }),
                })
                .unwrap();
            }
        }
        h.send(EngineCommand::SetSong(Box::new(SongSnapshot::compile(
            &project,
        ))))
        .unwrap();
        // 704 payload commands, more than the garbage queue holds (512): the engine must wait
        // for collection rather than free anything, and must not lose a command.
        run(&mut p, 200);
        let mut collected = 0;
        for _ in 0..8 {
            collected += h.collect_garbage();
            run(&mut p, 40);
        }
        collected += h.collect_garbage();
        assert_eq!(collected, MAX_CHANNELS * 10, "every params box comes back");
        for k in 0..40 {
            h.send(EngineCommand::NoteOn {
                slot: 5,
                key: 48 + k,
                velocity: 0.9,
            })
            .unwrap();
        }
        h.send(EngineCommand::NoteOff { slot: 5, key: 50 }).unwrap();
        h.send(EngineCommand::PreviewSample(Some(Arc::clone(&hat))))
            .unwrap();
        run(&mut p, 100);
        project.swing = 0.0;
        project.current_pattern_mut().steps = 32;
        h.send(EngineCommand::SetSong(Box::new(SongSnapshot::compile(
            &project,
        ))))
        .unwrap();
        h.send(EngineCommand::SetChannelSample {
            slot: 0,
            sample: None,
        })
        .unwrap();
        run(&mut p, 100);
        h.collect_garbage();

        // The demo mix (sends, sidechain, limiter) plus every effect kind on insert 10, then
        // parameter sweeps and bypass toggles while playing.
        let mut mix = Project::demo().mixer;
        for (k, kind) in EffectKind::ALL.into_iter().enumerate() {
            mix.strips[10].slots[k] = Some(EffectSlot::new(kind));
        }
        mix.strips[10].sidechain = Some(1);
        for (si, strip) in mix.strips.iter().enumerate() {
            for (k, slot) in strip.slots.iter().enumerate() {
                if let Some(slot) = slot {
                    h.send(EngineCommand::SetEffect {
                        strip: si as u8,
                        slot: k as u8,
                        effect: Some(create_effect(slot, sr as f32)),
                    })
                    .unwrap();
                }
            }
        }
        h.send(EngineCommand::SetMixer(Box::new(MixerParams::from_mixer(
            &mix,
        ))))
        .unwrap();
        for slot in 0..MAX_CHANNELS as u16 {
            h.send(EngineCommand::SetChannelParams {
                slot,
                params: Box::new(ChannelParams {
                    gain: 0.5,
                    route: (slot % 12) as u8,
                    ..ChannelParams::default()
                }),
            })
            .unwrap();
        }
        run(&mut p, 100);
        h.collect_garbage();
        for step in 0..20 {
            for k in 0..8_u8 {
                h.send(EngineCommand::SetEffectParam {
                    strip: 10,
                    slot: k,
                    index: step % 6,
                    value: step as f32 * 3.7 - 20.0,
                })
                .unwrap();
            }
            if let Some(s) = mix.strips[10].slots[step as usize % 8].as_mut() {
                s.enabled = !s.enabled;
            }
            h.send(EngineCommand::SetMixer(Box::new(MixerParams::from_mixer(
                &mix,
            ))))
            .unwrap();
            run(&mut p, 10);
            h.collect_garbage();
        }

        // Song mode: the demo arrangement with an audio clip on every track, automation of a
        // fader, a pan and an effect parameter, and tempo changes, played across loop wraps.
        let mut song = Project::demo();
        song.tempo = TempoMap::from_points_lossy(&[
            TempoPoint {
                at: Tick(0),
                bpm: 128.0,
            },
            TempoPoint {
                at: Tick(3840),
                bpm: 97.0,
            },
        ]);
        let tracks: Vec<_> = song.playlist.tracks.iter().map(|t| t.id).collect();
        for (k, &t) in tracks.iter().enumerate() {
            let c = song.playlist.add_clip(
                t,
                k as i64 * 700,
                3000,
                ClipKind::Audio {
                    source: SampleSource::BuiltIn(gt_core::BuiltInSample::Snare),
                    gain: 0.8,
                },
            );
            song.playlist.clip_mut(c).unwrap().offset = 200;
        }
        let (kick, bass) = (song.channels[0].id, song.channels[4].id);
        let targets = [
            ParamId::strip_pan(3),
            ParamId::Effect {
                strip: 10,
                slot: 2,
                kind: EffectKind::Delay,
                index: 1,
            },
            ParamId::Synth {
                channel: bass,
                param: SynthParam::Resonance,
            },
            ParamId::Channel {
                channel: kick,
                param: ChannelParam::Pitch,
            },
            ParamId::Strip {
                strip: 2,
                param: StripParam::Send(1),
            },
        ];
        song.mixer.strips[10].slots[2] = Some(EffectSlot::new(EffectKind::Delay));
        for target in targets {
            song.playlist.add_clip(
                tracks[5],
                0,
                7680,
                ClipKind::Automation(gt_core::Automation {
                    target,
                    points: vec![
                        gt_core::AutoPoint {
                            at: 0,
                            value: 0.0,
                            curve: gt_core::Curve::Bezier(0.4),
                        },
                        gt_core::AutoPoint::new(7680, 1.0),
                    ],
                }),
            );
        }
        // Modulators of every kind, some on automated parameters.
        for (k, shape) in LfoShape::ALL.into_iter().enumerate() {
            song.add_modulator(
                targets[k % targets.len()],
                ModSourceKind::Lfo {
                    shape,
                    rate: if k % 2 == 0 {
                        LfoRate::Sync(k)
                    } else {
                        LfoRate::Hz(3.0)
                    },
                    phase: 0.1,
                },
            );
        }
        song.add_modulator(
            ParamId::Synth {
                channel: bass,
                param: SynthParam::Cutoff,
            },
            ModSourceKind::default_follower(1),
        );
        for (slot, ch) in song.channels.iter().enumerate() {
            h.send(EngineCommand::SetChannelParams {
                slot: slot as u16,
                params: Box::new(ChannelParams::from_channel(ch, false)),
            })
            .unwrap();
        }
        h.send(EngineCommand::SetModulation(Box::new(ModPlan::compile(
            &song,
        ))))
        .unwrap();
        let snare = Arc::new(SampleData::mono(sr, gt_dsp::drums::snare(sr as f32)));
        let compiled = SongSnapshot::compile_song(&song, |_| Some(Arc::clone(&snare)));
        assert_eq!(compiled.audio.len(), tracks.len());
        assert_eq!(compiled.automation.len(), 2 + targets.len());
        h.send(EngineCommand::SetTempoMap(Box::new(song.tempo.clone())))
            .unwrap();
        h.send(EngineCommand::SetSong(Box::new(compiled))).unwrap();
        // Two bars (about 4.3 s) looped, played for 5 s: clips, tempo change and a wrap.
        h.send(EngineCommand::SetLoop(LoopRegion {
            start: Tick(0),
            end: Tick(7680),
            enabled: true,
        }))
        .unwrap();
        h.send(EngineCommand::Locate(Tick(0))).unwrap();
        run(&mut p, 5 * sr as usize / block);
        h.collect_garbage();
        // Edit a modulator while playing (state carries over), then remove them all.
        song.modulators[0].amount = -0.8;
        h.send(EngineCommand::SetModulation(Box::new(ModPlan::compile(
            &song,
        ))))
        .unwrap();
        run(&mut p, 50);
        song.modulators.clear();
        h.send(EngineCommand::SetModulation(Box::new(ModPlan::compile(
            &song,
        ))))
        .unwrap();
        run(&mut p, 50);
        assert_eq!(
            h.collect_garbage(),
            3,
            "two replaced plans and the empty one"
        );

        h.send(EngineCommand::Pause).unwrap();
        h.send(EngineCommand::Locate(Tick(12_345))).unwrap();
        h.send(EngineCommand::Play).unwrap();
        h.send(EngineCommand::FadeOut).unwrap();
        run(&mut p, 200);
        h.send(EngineCommand::Stop).unwrap();
        run(&mut p, 10);
        assert!(h
            .telemetry()
            .silent
            .load(std::sync::atomic::Ordering::Relaxed));
    }
}
