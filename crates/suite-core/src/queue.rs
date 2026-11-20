//! A job queue: jobs run one at a time on a worker thread and report progress over a
//! channel, so a UI or the CLI can follow them without blocking.

use std::{
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread::JoinHandle,
};

use suite_catalog::{Catalog, Channel};
use suite_install::{Event, Outcome, Stage};

use crate::Core;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    Install { app: String, channel: Option<Channel> },
    Update { app: String },
    Remove { app: String, purge: bool },
    Rollback { app: String },
}

impl Job {
    pub fn app(&self) -> &str {
        match self {
            Self::Install { app, .. }
            | Self::Update { app }
            | Self::Remove { app, .. }
            | Self::Rollback { app } => app,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobOutcome {
    Installed(Outcome),
    UpToDate,
    Removed { purged: Vec<std::path::PathBuf> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreEvent {
    Queued { id: u64, job: Job },
    Started { id: u64 },
    Stage { id: u64, stage: Stage },
    Progress { id: u64, done: u64, total: u64 },
    Finished { id: u64, outcome: JobOutcome },
    Failed { id: u64, error: String, kind: crate::ErrorKind },
}

impl CoreEvent {
    pub fn id(&self) -> u64 {
        match self {
            Self::Queued { id, .. }
            | Self::Started { id }
            | Self::Stage { id, .. }
            | Self::Progress { id, .. }
            | Self::Finished { id, .. }
            | Self::Failed { id, .. } => *id,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Finished { .. } | Self::Failed { .. })
    }
}

#[derive(Debug)]
pub struct Queue {
    jobs: Option<Sender<(u64, Job)>>,
    events_tx: Sender<CoreEvent>,
    events: Receiver<CoreEvent>,
    next: u64,
    worker: Option<JoinHandle<()>>,
}

impl Queue {
    /// Starts the worker. `catalog` is the verified catalog jobs plan against.
    pub fn start(core: Arc<Core>, catalog: Arc<Catalog>) -> Self {
        let (jobs_tx, jobs_rx) = mpsc::channel::<(u64, Job)>();
        let (events_tx, events) = mpsc::channel();
        let tx = events_tx.clone();
        let worker = std::thread::spawn(move || {
            for (id, job) in jobs_rx {
                let _ = tx.send(CoreEvent::Started { id });
                let event = match run(&core, &catalog, id, &job, &tx) {
                    Ok(outcome) => CoreEvent::Finished { id, outcome },
                    Err(e) => CoreEvent::Failed { id, error: e.to_string(), kind: e.kind() },
                };
                let _ = tx.send(event);
            }
        });
        Self { jobs: Some(jobs_tx), events_tx, events, next: 1, worker: Some(worker) }
    }

    /// Queues a job and returns its ID.
    pub fn submit(&mut self, job: Job) -> u64 {
        let id = self.next;
        self.next += 1;
        let _ = self.events_tx.send(CoreEvent::Queued { id, job: job.clone() });
        if let Some(tx) = &self.jobs {
            let _ = tx.send((id, job));
        }
        id
    }

    pub fn events(&self) -> &Receiver<CoreEvent> {
        &self.events
    }

    /// Hands the event stream to another thread (a UI's event pump). Afterwards
    /// [`Queue::events`] and [`Queue::wait`] see nothing.
    pub fn take_events(&mut self) -> Receiver<CoreEvent> {
        let (_, empty) = mpsc::channel();
        std::mem::replace(&mut self.events, empty)
    }

    /// Blocks until job `id` ends, passing every event to `on`.
    pub fn wait(&self, id: u64, on: &mut dyn FnMut(&CoreEvent)) -> Option<CoreEvent> {
        for ev in &self.events {
            on(&ev);
            if ev.id() == id && ev.is_terminal() {
                return Some(ev);
            }
        }
        None
    }

    /// Lets queued jobs finish and stops the worker.
    pub fn finish(mut self) {
        self.jobs = None;
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

fn run(
    core: &Core,
    catalog: &Catalog,
    id: u64,
    job: &Job,
    tx: &Sender<CoreEvent>,
) -> Result<JobOutcome, crate::CoreError> {
    let mut on = |e: Event| {
        let ev = match e {
            Event::Stage(stage) => CoreEvent::Stage { id, stage },
            Event::Download(p) => CoreEvent::Progress { id, done: p.done, total: p.total },
        };
        let _ = tx.send(ev);
    };
    match job {
        Job::Install { app, channel } => {
            core.install(catalog, app, *channel, &mut on).map(JobOutcome::Installed)
        }
        Job::Update { app } => {
            Ok(core.update(catalog, app, &mut on)?.map_or(JobOutcome::UpToDate, JobOutcome::Installed))
        }
        Job::Remove { app, purge } => {
            core.remove(Some(catalog), app, *purge).map(|purged| JobOutcome::Removed { purged })
        }
        Job::Rollback { app } => core.rollback(app).map(JobOutcome::Installed),
    }
}
