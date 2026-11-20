#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "byte counts as seconds and rates"
)]

//! The numbers behind a download's progress display: a smoothed speed, a steady time left,
//! the elapsed time and the peak speed.
//!
//! Pure logic with no clock of its own: the caller passes the time since the job started
//! with each reading, so the tests drive it with a fake clock. Feed it at a steady pace
//! (the UI does, from a timer) so a stall shows even when the job reports nothing.

use std::{collections::VecDeque, time::Duration};

/// The speed shown is the average over this many seconds.
const WINDOW: f64 = 3.0;
/// The time left uses the speed over a longer span, cut into slices: the mean of the
/// slices without the fastest and slowest tenth, so a burst or a hiccup does not move it.
const ETA_WINDOW: f64 = 10.0;
const ETA_SLICE: f64 = 1.0;
/// No progress for this long (after some progress) counts as stalled.
const STALL: f64 = 2.0;
/// The time left shows once the job has made progress for this long (again, after a
/// stall), so it does not start from a wild guess.
const ETA_AFTER: f64 = 1.0;
/// How quickly the shown time left follows a changed estimate (a time constant, seconds).
const ETA_TAU: f64 = 2.0;
/// A rise of the time left smaller than this fraction (plus `ETA_RISE_SECS`) waits
/// `ETA_RISE_WAIT` seconds before it shows, so the number does not flicker up and down.
const ETA_RISE: f64 = 0.25;
const ETA_RISE_SECS: f64 = 2.0;
const ETA_RISE_WAIT: f64 = 3.0;
/// The sparkline keeps one speed per step, `HISTORY` of them (12 s).
pub(crate) const HISTORY_STEP: f64 = 0.5;
pub(crate) const HISTORY: usize = 24;

/// Throughput readings for one job. See the module docs.
#[derive(Debug, Clone)]
pub(crate) struct Meter {
    /// `(seconds, bytes done)`, oldest first, covering at least `ETA_WINDOW`.
    samples: VecDeque<(f64, u64)>,
    now: f64,
    done: u64,
    total: u64,
    /// When `done` last grew.
    moved_at: f64,
    /// Since when the job has been making progress without a stall in between.
    active_since: f64,
    speed: f64,
    /// The steady speed over `ETA_WINDOW`, which the time left uses.
    eta_speed: f64,
    peak: f64,
    /// The smoothed estimate, and the rounded value on display.
    eta_smooth: Option<f64>,
    eta_shown: Option<f64>,
    /// When a small rise of the estimate was first seen.
    eta_rise_since: Option<f64>,
    history: [f64; HISTORY],
    /// The next slot to write, and how many slots hold a value.
    head: usize,
    filled: usize,
    next_step: f64,
}

impl Default for Meter {
    fn default() -> Self {
        Self::new()
    }
}

impl Meter {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            samples: VecDeque::from([(0.0, 0)]),
            now: 0.0,
            done: 0,
            total: 0,
            moved_at: 0.0,
            active_since: 0.0,
            speed: 0.0,
            eta_speed: 0.0,
            peak: 0.0,
            eta_smooth: None,
            eta_shown: None,
            eta_rise_since: None,
            history: [0.0; HISTORY],
            head: 0,
            filled: 0,
            next_step: HISTORY_STEP,
        }
    }

    /// Records that `done` of `total` bytes (0: unknown) are done `at` this time after the
    /// job started.
    pub(crate) fn update(&mut self, at: Duration, done: u64, total: u64) {
        let t = at.as_secs_f64().max(self.now);
        let dt = t - self.now;
        if done > self.done {
            if self.done == 0 || self.stalled() {
                // Starting, or resuming after a stall: measure the new pace on its own,
                // from the last reading before it.
                self.samples.clear();
                self.samples.push_back((self.now, self.done));
                self.active_since = self.now;
                self.eta_smooth = None;
                self.eta_shown = None;
                self.eta_rise_since = None;
            }
            self.moved_at = t;
        }
        self.now = t;
        self.done = done.max(self.done);
        self.total = total;
        self.samples.push_back((t, self.done));
        self.measure_speed();
        self.record_history();
        self.estimate(dt);
    }

    fn measure_speed(&mut self) {
        let t = self.now;
        let oldest = (t - ETA_WINDOW).max(self.active_since);
        while self.samples.len() > 2 && self.samples[1].0 <= oldest {
            self.samples.pop_front();
        }
        if self.done == 0 {
            self.speed = 0.0;
            self.eta_speed = 0.0;
            return;
        }
        if let Some(speed) = self.rate_over(WINDOW) {
            self.speed = speed;
        }
        if let Some(speed) = self.steady_rate() {
            self.eta_speed = speed;
        }
        // Peaks of the first moments are noise.
        if t - self.active_since >= ETA_AFTER && !self.stalled() {
            self.peak = self.peak.max(self.speed);
        }
    }

    /// Bytes done at time `x` (within the samples), interpolated between readings.
    fn done_at(&self, x: f64) -> f64 {
        let i = self.samples.partition_point(|&(st, _)| st <= x).max(1) - 1;
        let (t0, d0) = self.samples[i];
        match self.samples.get(i + 1) {
            Some(&(t1, d1)) if t1 > t0 => {
                d0 as f64 + (d1 - d0) as f64 * ((x - t0) / (t1 - t0)).clamp(0.0, 1.0)
            }
            _ => d0 as f64,
        }
    }

    /// The earliest time a window of `window` seconds may reach back to: not before the
    /// job got going (again), nor before the oldest sample.
    fn window_start(&self, window: f64) -> f64 {
        (self.now - window).max(self.active_since).max(self.samples[0].0)
    }

    /// Bytes per second over the last `window` seconds (or since the job got going, when
    /// that is more recent); `None` over too short a span to tell.
    fn rate_over(&self, window: f64) -> Option<f64> {
        let (t, from) = (self.now, self.window_start(window));
        (t - from >= 0.05).then(|| ((self.done as f64 - self.done_at(from)) / (t - from)).max(0.0))
    }

    /// The speed over `ETA_WINDOW` in `ETA_SLICE`s, without the fastest and slowest tenth
    /// of them (once there are five).
    fn steady_rate(&self) -> Option<f64> {
        let (t, from) = (self.now, self.window_start(ETA_WINDOW));
        let n = ((t - from) / ETA_SLICE + 1e-9).floor() as usize;
        if n == 0 {
            return self.rate_over(ETA_WINDOW);
        }
        let mut rates: Vec<f64> = (0..n)
            .map(|k| {
                let end = t - k as f64 * ETA_SLICE;
                (self.done_at(end) - self.done_at(end - ETA_SLICE)) / ETA_SLICE
            })
            .collect();
        rates.sort_by(f64::total_cmp);
        let cut = if n >= 5 { (n / 10).max(1) } else { 0 };
        let kept = &rates[cut..n - cut];
        Some(kept.iter().sum::<f64>() / kept.len() as f64)
    }

    fn record_history(&mut self) {
        let mut steps = 0;
        while self.now >= self.next_step {
            self.next_step += HISTORY_STEP;
            if self.done == 0 {
                continue;
            }
            steps += 1;
            if steps <= HISTORY {
                self.history[self.head] = self.speed;
                self.head = (self.head + 1) % HISTORY;
                self.filled = (self.filled + 1).min(HISTORY);
            }
        }
    }

    fn estimate(&mut self, dt: f64) {
        let fresh = self.now - self.active_since;
        if self.total == 0
            || self.done >= self.total
            || self.done == 0
            || self.eta_speed <= 0.0
            || self.stalled()
            || fresh < ETA_AFTER
        {
            self.eta_smooth = None;
            self.eta_shown = None;
            self.eta_rise_since = None;
            return;
        }
        let raw = (self.total - self.done) as f64 / self.eta_speed;
        let smooth = match self.eta_smooth {
            None => raw,
            Some(previous) => {
                // The old estimate counts down by itself; it moves towards the new one.
                let predicted = (previous - dt).max(0.0);
                let alpha = 1.0 - (-dt / ETA_TAU).exp();
                predicted + (raw - predicted) * alpha
            }
        };
        self.eta_smooth = Some(smooth);
        let candidate = round_eta(smooth);
        self.eta_shown = match self.eta_shown {
            Some(shown) if candidate > shown => {
                let big = candidate >= shown * (1.0 + ETA_RISE) + ETA_RISE_SECS;
                let since = *self.eta_rise_since.get_or_insert(self.now);
                if big || self.now - since >= ETA_RISE_WAIT {
                    self.eta_rise_since = None;
                    Some(candidate)
                } else {
                    Some(shown)
                }
            }
            _ => {
                self.eta_rise_since = None;
                Some(candidate)
            }
        };
    }

    /// How much is done, 0–1; `None` while the total is unknown.
    #[must_use]
    pub(crate) fn fraction(&self) -> Option<f64> {
        (self.total > 0).then(|| (self.done as f64 / self.total as f64).clamp(0.0, 1.0))
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn done(&self) -> u64 {
        self.done
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn total(&self) -> u64 {
        self.total
    }

    /// Whether everything is done (the total is known and reached).
    #[must_use]
    pub(crate) fn complete(&self) -> bool {
        self.total > 0 && self.done >= self.total
    }

    /// Bytes per second over the last few seconds; `None` before there is any progress.
    #[must_use]
    pub(crate) fn speed(&self) -> Option<f64> {
        (self.done > 0).then_some(self.speed)
    }

    /// The highest speed so far (after the first second of progress).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn peak(&self) -> f64 {
        self.peak
    }

    /// Seconds left, rounded for display; `None` while it is not known well enough.
    #[must_use]
    pub(crate) fn eta(&self) -> Option<f64> {
        self.eta_shown
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn elapsed(&self) -> Duration {
        Duration::from_secs_f64(self.now)
    }

    /// No progress for a while, after some progress, with work left.
    #[must_use]
    pub(crate) fn stalled(&self) -> bool {
        self.done > 0 && !self.complete() && self.now - self.moved_at >= STALL
    }

    /// The recent speeds, oldest first, one per `HISTORY_STEP`, at most `HISTORY`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn history(&self) -> Vec<f64> {
        (0..self.filled).map(|i| self.history[(self.head + HISTORY - self.filled + i) % HISTORY]).collect()
    }
}

/// Rounds seconds left to a step that suits their size: whole seconds below 10, then 5 s,
/// 10 s, whole minutes and five minutes. Never rounds down to nothing.
#[must_use]
pub(crate) fn round_eta(secs: f64) -> f64 {
    let step = if secs < 10.0 {
        1.0
    } else if secs < 60.0 {
        5.0
    } else if secs < 600.0 {
        10.0
    } else if secs < 3600.0 {
        60.0
    } else {
        300.0
    };
    ((secs / step).round() * step).max(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1_000_000;
    const TICK: f64 = 0.25;

    /// Drives a meter with a fake clock: ticks every 250 ms from `from` to `to` seconds,
    /// `rate(t)` bytes per second, and returns what `each` saw at every tick.
    fn run(
        m: &mut Meter,
        done: &mut u64,
        total: u64,
        (from, to): (f64, f64),
        rate: impl Fn(f64) -> u64,
        mut each: impl FnMut(f64, &Meter),
    ) {
        let mut t = from;
        while t < to - 1e-9 {
            t += TICK;
            let next = *done + (rate(t) as f64 * TICK) as u64;
            *done = if total == 0 { next } else { next.min(total) };
            m.update(Duration::from_secs_f64(t), *done, total);
            each(t, m);
        }
    }

    fn near(a: f64, b: f64, tolerance: f64) -> bool {
        (a - b).abs() <= tolerance * b.abs().max(1.0)
    }

    #[test]
    fn steady_rate() {
        let mut m = Meter::new();
        let mut done = 0;
        let total = 100 * MB;
        let mut shown = Vec::new();
        run(
            &mut m,
            &mut done,
            total,
            (0.0, 6.0),
            |_| 10 * MB,
            |t, m| {
                if t < ETA_AFTER {
                    assert_eq!(m.eta(), None, "no time left before a second at {t}");
                }
                if let Some(e) = m.eta() {
                    shown.push((t, e));
                }
            },
        );
        let speed = m.speed().unwrap();
        assert!(near(speed, 10e6, 0.01), "{speed}");
        assert!(near(m.peak(), 10e6, 0.01));
        // Shows from about a second in, always close to the truth, and only counts down.
        assert!(shown.first().unwrap().0 <= ETA_AFTER + TICK + 1e-9);
        for &(t, e) in &shown {
            let truth = 10.0 - t;
            assert!((e - truth).abs() <= 1.0, "{e} at {t}");
        }
        assert!(shown.windows(2).all(|w| w[1].1 <= w[0].1), "{shown:?}");
        assert_eq!(m.fraction(), Some(0.6));
        assert_eq!(m.elapsed(), Duration::from_secs(6));
        assert!(!m.stalled());
        // A full history of the steady speed.
        let h = m.history();
        assert_eq!(h.len(), 12);
        assert!(h.iter().skip(6).all(|&v| near(v, 10e6, 0.01)), "{h:?}");
    }

    #[test]
    fn stall_then_resume() {
        let mut m = Meter::new();
        let mut done = 0;
        let total = 200 * MB;
        run(&mut m, &mut done, total, (0.0, 4.0), |_| 10 * MB, |_, _| {});
        assert!(m.eta().is_some());
        let mut flagged = None;
        run(
            &mut m,
            &mut done,
            total,
            (4.0, 10.0),
            |_| 0,
            |t, m| {
                if m.stalled() && flagged.is_none() {
                    flagged = Some(t);
                }
                if t >= 4.0 + STALL {
                    assert_eq!(m.eta(), None, "no time left while stalled ({t})");
                }
            },
        );
        assert!(near(flagged.unwrap(), 4.0 + STALL, 0.05), "{flagged:?}");
        assert_eq!(m.speed(), Some(0.0), "the speed drops to nothing");
        assert!(near(m.peak(), 10e6, 0.01), "the peak stays");
        // Back at 20 MB/s: the speed follows at once, not after the stall leaves the
        // window, and the time left comes back after a second.
        let mut back = None;
        run(
            &mut m,
            &mut done,
            total,
            (10.0, 12.0),
            |_| 20 * MB,
            |t, m| {
                assert!(!m.stalled());
                if m.eta().is_some() && back.is_none() {
                    back = Some(t);
                }
            },
        );
        assert!(near(m.speed().unwrap(), 20e6, 0.01));
        let back = back.unwrap();
        assert!(back <= 10.0 + ETA_AFTER + TICK + 1e-9, "{back}");
        let truth = (total - done) as f64 / 20e6;
        assert!((m.eta().unwrap() - truth).abs() <= 1.0, "{:?} vs {truth}", m.eta());
        // The sparkline shows the dip.
        let h = m.history();
        assert!(h.contains(&0.0), "{h:?}");
    }

    #[test]
    fn burst() {
        let mut m = Meter::new();
        let mut done = 0;
        let total = 1000 * MB;
        run(&mut m, &mut done, total, (0.0, 12.0), |_| 10 * MB, |_, _| {});
        let before = m.eta().unwrap();
        let mut speeds = Vec::new();
        let mut shown = vec![before];
        // One tick delivers 30 MB extra (a block flushed at once), then the pace goes on.
        done += 30 * MB;
        run(
            &mut m,
            &mut done,
            total,
            (12.0, 30.0),
            |_| 10 * MB,
            |_, m| {
                speeds.push(m.speed().unwrap());
                shown.push(m.eta().unwrap());
                // Always close to the truth at the steady pace: the burst barely moves it.
                let truth = (m.total() - m.done()) as f64 / 10e6;
                assert!((m.eta().unwrap() - truth).abs() <= 0.05 * truth + 5.0, "{:?} vs {truth}", m.eta());
            },
        );
        // The speed rises by the burst spread over the window, not by 30 MB per tick.
        let top = speeds.iter().copied().fold(0.0, f64::max);
        assert!(top <= 10e6 + 30e6 / WINDOW + 1e6, "{top}");
        assert!(near(*speeds.last().unwrap(), 10e6, 0.01));
        // The time left never flickers: it turns from falling to rising at most once, in
        // small steps.
        let moves: Vec<f64> = shown.windows(2).map(|w| w[1] - w[0]).filter(|&d| d != 0.0).collect();
        let turns = moves.windows(2).filter(|d| d[0] < 0.0 && d[1] > 0.0).count();
        assert!(turns <= 1, "{shown:?}");
        for w in shown.windows(2) {
            assert!(w[1] <= w[0] * (1.0 + ETA_RISE) + ETA_RISE_SECS, "{shown:?}");
        }
        let truth = (total - done) as f64 / 10e6;
        assert!(near(m.eta().unwrap(), truth, 0.1), "{:?} vs {truth}", m.eta());
    }

    #[test]
    fn unknown_total() {
        let mut m = Meter::new();
        let mut done = 0;
        run(
            &mut m,
            &mut done,
            0,
            (0.0, 5.0),
            |_| 4 * MB,
            |_, m| {
                assert_eq!(m.fraction(), None);
                assert_eq!(m.eta(), None);
                assert!(!m.complete());
            },
        );
        assert!(near(m.speed().unwrap(), 4e6, 0.01));
        assert_eq!(m.done(), 20 * MB);
        assert_eq!(m.history().len(), 10);
    }

    #[test]
    fn nothing_yet() {
        let mut m = Meter::new();
        m.update(Duration::from_secs(3), 0, 10 * MB);
        assert_eq!(m.speed(), None);
        assert_eq!(m.eta(), None);
        assert!(!m.stalled(), "not stalled before it started");
        assert_eq!(m.history(), Vec::<f64>::new());
        assert_eq!(m.fraction(), Some(0.0));
        // A slow start (opening the archive) does not drag the speed down.
        m.update(Duration::from_secs_f64(3.5), 5 * MB / 2, 10 * MB);
        assert!(near(m.speed().unwrap(), 5e6, 0.01));
    }

    #[test]
    fn completion() {
        let mut m = Meter::new();
        let mut done = 0;
        let total = 30 * MB;
        run(&mut m, &mut done, total, (0.0, 4.0), |_| 10 * MB, |_, _| {});
        assert!(m.complete());
        assert_eq!(m.fraction(), Some(1.0));
        assert_eq!(m.eta(), None);
        assert!(!m.stalled());
        // Finishing (renames, closing files) takes a while; that is not a stall.
        m.update(Duration::from_secs(9), total, total);
        assert!(!m.stalled());
        assert_eq!(m.elapsed(), Duration::from_secs(9));
    }

    #[test]
    fn rounding() {
        assert_eq!(round_eta(0.2), 1.0);
        assert_eq!(round_eta(7.4), 7.0);
        assert_eq!(round_eta(23.0), 25.0);
        assert_eq!(round_eta(124.0), 120.0);
        assert_eq!(round_eta(1000.0), 1020.0);
        assert_eq!(round_eta(4000.0), 3900.0);
    }
}
