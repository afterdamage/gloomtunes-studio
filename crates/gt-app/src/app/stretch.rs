//! The stretch-render cache (Prompt 15, D106): renders of stretched audio clips, made on a
//! worker thread and keyed by everything that shapes them, so editing a clip back to earlier
//! settings finds its render again while it is still cached.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::sync::Arc;

use gt_core::{ClipKind, Project, SampleData, SampleSource};
use gt_engine::stretch::{self, StretchJob};

use super::GloomApp;

type Done = (u64, SampleData);

/// Renders ready and in progress.
pub(super) struct StretchCache {
    ready: HashMap<u64, Arc<SampleData>>,
    pending: HashSet<u64>,
    /// Keys of sampler time-stretch renders asked for (kept when clips are re-requested).
    fits: HashSet<u64>,
    /// Key of the browser's tempo-synced preview render (Prompt 25), kept while it is current.
    preview: Option<u64>,
    jobs: mpsc::Sender<(StretchJob, Arc<SampleData>)>,
    done: mpsc::Receiver<Done>,
}

impl StretchCache {
    pub(super) fn new() -> Self {
        let (jobs, rx) = mpsc::channel::<(StretchJob, Arc<SampleData>)>();
        let (tx, done) = mpsc::channel::<Done>();
        std::thread::Builder::new()
            .name("gt-stretch".into())
            .spawn(move || {
                while let Ok((job, src)) = rx.recv() {
                    if tx.send((job.key, job.render(&src))).is_err() {
                        return;
                    }
                }
            })
            .expect("spawn the stretch worker");
        Self {
            ready: HashMap::new(),
            pending: HashSet::new(),
            fits: HashSet::new(),
            preview: None,
            jobs,
            done,
        }
    }

    /// A ready render.
    pub(super) fn get(&self, key: u64) -> Option<Arc<SampleData>> {
        self.ready.get(&key).map(Arc::clone)
    }

    /// Queues the renders `project` needs that are neither ready nor queued, and forgets the
    /// ready ones it no longer uses.
    fn request(
        &mut self,
        project: &Project,
        samples: impl Fn(&SampleSource) -> Option<Arc<SampleData>>,
    ) {
        let mut used = HashSet::new();
        for c in &project.playlist.clips {
            let ClipKind::Audio { source, .. } = &c.kind else {
                continue;
            };
            let Some(sample) = samples(source) else {
                continue;
            };
            let Some(key) = stretch::key(project, c, &sample) else {
                continue;
            };
            used.insert(key);
            if self.ready.contains_key(&key) || self.pending.contains(&key) {
                continue;
            }
            if let Some(job) = stretch::job(project, c, &sample) {
                self.pending.insert(key);
                let _ = self.jobs.send((job, sample));
            }
        }
        self.ready
            .retain(|k, _| used.contains(k) || self.fits.contains(k) || self.preview == Some(*k));
    }

    /// The time-stretch render of sampler `s` (Prompt 23): ready, or queued now and `None`
    /// until it is (the channel plays its sample resampled meanwhile).
    pub(super) fn fitted(
        &mut self,
        project: &Project,
        s: &gt_core::SamplerSettings,
        samples: impl Fn(&SampleSource) -> Option<Arc<SampleData>>,
    ) -> Option<Arc<SampleData>> {
        let sample = samples(s.sample.as_ref()?)?;
        let job = stretch::fit_job(project, s, &sample)?;
        self.fits.insert(job.key);
        if let Some(r) = self.ready.get(&job.key) {
            return Some(Arc::clone(r));
        }
        if self.pending.insert(job.key) {
            let _ = self.jobs.send((job, sample));
        }
        None
    }

    /// The browser preview's render: ready, or queued now and `None` until it is. Only the
    /// latest preview's render is kept.
    pub(super) fn preview(
        &mut self,
        job: StretchJob,
        sample: Arc<SampleData>,
    ) -> Option<Arc<SampleData>> {
        if let Some(old) = self.preview.replace(job.key) {
            if old != job.key {
                self.ready.remove(&old);
            }
        }
        if let Some(r) = self.ready.get(&job.key) {
            return Some(Arc::clone(r));
        }
        if self.pending.insert(job.key) {
            let _ = self.jobs.send((job, sample));
        }
        None
    }

    /// Takes finished renders; their keys if any arrived.
    fn poll(&mut self) -> Option<Vec<u64>> {
        let mut keys = Vec::new();
        while let Ok((key, data)) = self.done.try_recv() {
            self.pending.remove(&key);
            self.ready.insert(key, Arc::new(data));
            keys.push(key);
        }
        (!keys.is_empty()).then_some(keys)
    }
}

impl GloomApp {
    /// Queues the stretch renders the playlist needs.
    pub(super) fn request_stretches(&mut self) {
        let lib = &self.library;
        self.stretch.request(&self.project, |src| {
            lib.get(src).map(|l| Arc::clone(&l.data))
        });
    }

    /// Picks up finished renders and replays the song with them.
    pub(super) fn poll_stretches(&mut self) {
        let Some(keys) = self.stretch.poll() else {
            return;
        };
        if self.transport.song_mode {
            self.push_song();
        }
        // Samplers in time-stretch mode whose render just arrived.
        if keys.iter().any(|k| self.stretch.fits.contains(k)) {
            for i in 0..self.project.channels.len() {
                if self.project.channels[i]
                    .sampler()
                    .is_some_and(|s| s.stretch_beats > 0.0)
                {
                    self.push_sample(i);
                }
            }
        }
    }
}
