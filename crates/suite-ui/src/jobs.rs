//! The jobs the window follows, built from the core queue's events: their stage, smoothed
//! speed and time left (`meter`), outcome or plain error. No clock of its own: callers
//! pass the time, so tests drive it with a fake one.

use std::{collections::HashMap, time::Duration};

use suite_core::{CoreEvent, ErrorKind, Job, JobOutcome};
use suite_install::{Outcome, Stage};

use crate::{
    format,
    i18n::tr,
    meter::Meter,
    view::{Busy, Tone},
    words,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Queued,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone)]
struct Entry {
    id: u64,
    job: Job,
    name: String,
    state: State,
    stage: Option<Stage>,
    /// When it started, on the caller's clock.
    started: Option<Duration>,
    meter: Meter,
    /// The newest progress, fed to the meter on each tick.
    progress: Option<(u64, u64)>,
    outcome: Option<JobOutcome>,
    error: Option<(ErrorKind, String)>,
}

/// What a job card shows.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct JobView {
    pub id: u64,
    pub app: String,
    pub title: String,
    pub stage: String,
    /// 0–1, below 0 when unknown.
    pub fraction: f32,
    pub detail: String,
    pub state: State,
    pub message: String,
    pub error_detail: String,
    pub can_open: bool,
    pub can_retry: bool,
    pub tone: Tone,
}

#[derive(Debug, Default)]
pub(crate) struct Jobs {
    list: Vec<Entry>,
}

impl Jobs {
    /// Takes an event from the queue; `name` gives an app's display name.
    pub(crate) fn apply(&mut self, ev: &CoreEvent, now: Duration, name: &dyn Fn(&str) -> String) {
        match ev {
            CoreEvent::Queued { id, job } => {
                self.list.retain(|e| e.id != *id);
                self.list.push(Entry {
                    id: *id,
                    name: name(job.app()),
                    job: job.clone(),
                    state: State::Queued,
                    stage: None,
                    started: None,
                    meter: Meter::new(),
                    progress: None,
                    outcome: None,
                    error: None,
                });
            }
            CoreEvent::Started { id } => {
                if let Some(e) = self.get_mut(*id) {
                    e.state = State::Running;
                    e.started = Some(now);
                }
            }
            CoreEvent::Stage { id, stage } => {
                if let Some(e) = self.get_mut(*id) {
                    e.stage = Some(*stage);
                }
            }
            CoreEvent::Progress { id, done, total } => self.progress(*id, *done, *total),
            CoreEvent::Finished { id, outcome } => {
                if let Some(e) = self.get_mut(*id) {
                    e.state = State::Done;
                    e.outcome = Some(outcome.clone());
                }
            }
            CoreEvent::Failed { id, error, kind } => {
                if let Some(e) = self.get_mut(*id) {
                    e.state = State::Failed;
                    e.error = Some((*kind, error.clone()));
                }
            }
        }
    }

    /// Notes the newest progress; the meter takes it on the next [`Jobs::tick`].
    pub(crate) fn progress(&mut self, id: u64, done: u64, total: u64) {
        if let Some(e) = self.get_mut(id) {
            e.progress = Some((done, total));
        }
    }

    /// Feeds every running download's meter at a steady pace, so a stall shows even when
    /// nothing arrives.
    pub(crate) fn tick(&mut self, now: Duration) {
        for e in &mut self.list {
            if let (State::Running, Some(started), Some((done, total))) = (e.state, e.started, e.progress) {
                e.meter.update(now.saturating_sub(started), done, total);
            }
        }
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut Entry> {
        self.list.iter_mut().find(|e| e.id == id)
    }

    pub(crate) fn job(&self, id: u64) -> Option<&Job> {
        self.list.iter().find(|e| e.id == id).map(|e| &e.job)
    }

    /// Queued or running.
    pub(crate) fn active(&self) -> usize {
        self.list.iter().filter(|e| matches!(e.state, State::Queued | State::Running)).count()
    }

    /// Forgets a finished or failed job.
    pub(crate) fn dismiss(&mut self, id: u64) {
        self.list.retain(|e| e.id != id || matches!(e.state, State::Queued | State::Running));
    }

    /// The apps with a job under way, and how far it is.
    pub(crate) fn busy(&self) -> HashMap<String, Busy> {
        self.list
            .iter()
            .filter(|e| matches!(e.state, State::Queued | State::Running))
            .map(|e| (e.job.app().to_owned(), Busy { fraction: fraction(e), stage: stage_text(e) }))
            .collect()
    }

    /// Newest first.
    pub(crate) fn views(&self) -> Vec<JobView> {
        self.list.iter().rev().map(view).collect()
    }
}

#[expect(clippy::cast_possible_truncation, clippy::cast_precision_loss, reason = "a progress bar")]
fn fraction(e: &Entry) -> f32 {
    match (e.state, e.stage) {
        // Done, or past the download: the rest takes moments.
        (State::Done, _)
        | (State::Running, Some(Stage::Unpack | Stage::Switch | Stage::Integrate | Stage::Cleanup)) => 1.0,
        (State::Running, Some(Stage::Download) | None) => e
            .meter
            .fraction()
            .or_else(|| e.progress.filter(|p| p.1 > 0).map(|p| p.0 as f64 / p.1 as f64))
            .map_or(-1.0, |f| f as f32),
        _ => -1.0,
    }
}

fn stage_text(e: &Entry) -> String {
    match (e.state, e.stage) {
        (State::Queued, _) => tr!("Waiting"),
        (State::Running, Some(s)) => words::stage(s),
        (State::Running, None) => tr!("Starting"),
        (State::Done, _) => tr!("Done"),
        (State::Failed, _) => tr!("Failed"),
    }
}

fn title(e: &Entry) -> String {
    let n = &e.name;
    let done = e.state == State::Done;
    match (&e.job, &e.outcome) {
        (_, Some(JobOutcome::Installed(o))) if o.opened_store.is_some() => {
            o.opened_store.map_or_default(|store| words::opened_in_store(n, store))
        }
        (_, Some(JobOutcome::UpToDate)) => tr!("{} is up to date", n),
        (Job::Install { .. }, Some(JobOutcome::Installed(o))) if o.from.is_some() => {
            tr!("Updated {} to {}", n, o.to)
        }
        (Job::Install { .. }, Some(JobOutcome::Installed(o))) => tr!("Installed {} {}", n, o.to),
        (Job::Update { .. }, Some(JobOutcome::Installed(o))) => tr!("Updated {} to {}", n, o.to),
        (Job::Rollback { .. }, Some(JobOutcome::Installed(o))) => tr!("{} is back on {}", n, o.to),
        (Job::Remove { .. }, _) if done => tr!("Removed {}", n),
        (Job::Install { .. }, _) if e.state == State::Failed => tr!("{} wasn't installed", n),
        (Job::Update { .. }, _) if e.state == State::Failed => tr!("{} wasn't updated", n),
        (Job::Remove { .. }, _) if e.state == State::Failed => tr!("{} wasn't removed", n),
        (Job::Rollback { .. }, _) if e.state == State::Failed => tr!("{} didn't go back", n),
        (Job::Install { .. }, _) => tr!("Installing {}", n),
        (Job::Update { .. }, _) => tr!("Updating {}", n),
        (Job::Remove { .. }, _) => tr!("Removing {}", n),
        (Job::Rollback { .. }, _) => tr!("Going back to the previous {}", n),
    }
}

fn detail(e: &Entry) -> String {
    if e.state != State::Running || !matches!(e.stage, Some(Stage::Download)) {
        return String::new();
    }
    let m = &e.meter;
    let Some((done, total)) = e.progress else { return String::new() };
    let mut parts = vec![if total > 0 {
        tr!("{} of {}", format::bytes(done), format::bytes(total))
    } else {
        format::bytes(done)
    }];
    if m.stalled() {
        parts.push(tr!("waiting for the server"));
    } else if let Some(speed) = m.speed().filter(|s| *s > 0.0) {
        parts.push(format::rate(speed));
        if let Some(eta) = m.eta() {
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "rounded seconds")]
            parts.push(tr!("{} left", format::duration(eta as u64)));
        }
    }
    parts.join(" · ")
}

fn view(e: &Entry) -> JobView {
    let installed = matches!(&e.outcome, Some(JobOutcome::Installed(o)) if o.opened_store.is_none());
    let restart = matches!(&e.outcome, Some(JobOutcome::Installed(o)) if o.needs_restart);
    let (message, error_detail) = match (&e.error, &e.outcome) {
        (Some((kind, detail)), _) => (words::error(*kind, &e.name), detail.clone()),
        // Handed to a store or the app's website: what happens next.
        (None, Some(JobOutcome::Installed(Outcome { opened_store: Some(store), .. }))) => {
            (words::store_takes_over(*store), String::new())
        }
        (None, _) => (String::new(), String::new()),
    };
    let stage = if restart {
        tr!("{} is open: the new version starts next time you open it", e.name)
    } else {
        stage_text(e)
    };
    JobView {
        id: e.id,
        app: e.job.app().to_owned(),
        title: title(e),
        stage,
        fraction: fraction(e),
        detail: detail(e),
        state: e.state,
        message,
        error_detail,
        can_open: e.state == State::Done && installed && !matches!(e.job, Job::Remove { .. }),
        can_retry: e.state == State::Failed,
        tone: match e.state {
            State::Done => Tone::Ok,
            State::Failed => Tone::Bad,
            _ => Tone::Normal,
        },
    }
}

#[cfg(test)]
mod tests {
    use suite_install::Outcome;

    use super::*;

    const MB: u64 = 1_000_000;

    fn name(id: &str) -> String {
        let mut c = id.chars();
        c.next().map_or_default(|f| f.to_uppercase().chain(c).collect())
    }

    fn at(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    fn install(jobs: &mut Jobs, id: u64, app: &str) {
        jobs.apply(
            &CoreEvent::Queued { id, job: Job::Install { app: app.into(), channel: None } },
            at(0),
            &name,
        );
    }

    #[test]
    fn a_download_shows_speed_and_time_left() {
        let mut jobs = Jobs::default();
        install(&mut jobs, 1, "specular");
        assert_eq!(jobs.views()[0].title, "Installing Specular");
        assert_eq!(jobs.busy()["specular"].stage, "Waiting");
        jobs.apply(&CoreEvent::Started { id: 1 }, at(0), &name);
        jobs.apply(&CoreEvent::Stage { id: 1, stage: Stage::Download }, at(0), &name);
        // 10 MB/s for 3 s of a 100 MB download, ticking every 250 ms.
        for t in 1..=12 {
            jobs.progress(1, t * 10 * MB / 4, 100 * MB);
            jobs.tick(at(t * 250));
        }
        let v = &jobs.views()[0];
        assert_eq!(v.stage, "Downloading");
        assert!((v.fraction - 0.3).abs() < 0.01, "{}", v.fraction);
        assert!(v.detail.starts_with("30 MB of 100 MB · 10 MB/s · "), "{}", v.detail);
        assert!(v.detail.ends_with(" s left"), "{}", v.detail);
        assert_eq!(jobs.active(), 1);

        // Nothing arrives for three seconds: it says so instead of a stale speed.
        for t in 13..=24 {
            jobs.tick(at(t * 250));
        }
        assert!(jobs.views()[0].detail.ends_with("waiting for the server"), "{}", jobs.views()[0].detail);

        jobs.apply(&CoreEvent::Stage { id: 1, stage: Stage::Unpack }, at(7000), &name);
        assert_eq!(jobs.views()[0].fraction, 1.0);
        let outcome = Outcome {
            app: "specular".into(),
            from: None,
            to: "1.2.0".parse().unwrap(),
            needs_restart: false,
            unchanged: false,
            opened_store: None,
        };
        jobs.apply(&CoreEvent::Finished { id: 1, outcome: JobOutcome::Installed(outcome) }, at(8000), &name);
        let v = &jobs.views()[0];
        assert_eq!(
            (v.title.as_str(), v.stage.as_str(), v.can_open),
            ("Installed Specular 1.2.0", "Done", true)
        );
        assert_eq!(jobs.active(), 0);
        assert_eq!(jobs.busy().len(), 0);
        jobs.dismiss(1);
        assert_eq!(jobs.views().len(), 0);
    }

    #[test]
    fn failures_are_plain_and_can_be_retried() {
        let mut jobs = Jobs::default();
        install(&mut jobs, 3, "zynk");
        jobs.apply(&CoreEvent::Started { id: 3 }, at(0), &name);
        let error = "zynk released a new build; the catalog needs refreshing before it can be installed (nothing was changed)";
        jobs.apply(
            &CoreEvent::Failed { id: 3, error: error.into(), kind: ErrorKind::NeedsRefresh },
            at(10),
            &name,
        );
        let v = &jobs.views()[0];
        assert!(
            v.message.starts_with("Zynk released a new build that the app list doesn't know yet"),
            "{}",
            v.message
        );
        assert_eq!(v.error_detail, error);
        assert!(v.can_retry && !v.can_open);
        assert_eq!(v.title, "Zynk wasn't installed");
        assert_eq!(v.tone, Tone::Bad);
        assert_eq!(jobs.job(3), Some(&Job::Install { app: "zynk".into(), channel: None }));
    }

    #[test]
    fn titles_follow_the_job() {
        let mut jobs = Jobs::default();
        jobs.apply(
            &CoreEvent::Queued { id: 1, job: Job::Remove { app: "harbor".into(), purge: true } },
            at(0),
            &name,
        );
        jobs.apply(&CoreEvent::Queued { id: 2, job: Job::Update { app: "bindery".into() } }, at(0), &name);
        assert_eq!(jobs.views()[0].title, "Updating Bindery");
        assert_eq!(jobs.views()[1].title, "Removing Harbor");
        jobs.apply(&CoreEvent::Finished { id: 2, outcome: JobOutcome::UpToDate }, at(0), &name);
        jobs.apply(
            &CoreEvent::Finished { id: 1, outcome: JobOutcome::Removed { purged: Vec::new() } },
            at(0),
            &name,
        );
        assert_eq!(jobs.views()[0].title, "Bindery is up to date");
        assert_eq!(jobs.views()[1].title, "Removed Harbor");
        assert!(!jobs.views()[1].can_open);
        // Running jobs can't be dismissed.
        jobs.apply(&CoreEvent::Queued { id: 4, job: Job::Rollback { app: "harbor".into() } }, at(0), &name);
        jobs.dismiss(4);
        assert_eq!(jobs.active(), 1);
    }
}
