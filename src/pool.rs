//! Open GDAL handles on one file, shared by all workers, after the outdoor
//! map's `HillshadingDatasets`.

use crate::error::AppError;
use gdal::Dataset;
use std::{
    path::{Path, PathBuf},
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

const POISONED: &str = "dataset pool mutex not poisoned";

/// A handle carries the part of the file's tile index it has touched, so
/// handles are capped per file and closed once idle; reopening one whose
/// index pages are in the page cache is cheap.
pub struct Pool {
    path: PathBuf,
    state: Mutex<State>,
    returned: Condvar,
    max_open: usize,
}

struct State {
    idle: Vec<(Dataset, Instant)>,
    /// Idle plus checked-out handles.
    open: usize,
    stats: Stats,
}

/// Counters since the last [`Pool::take_stats`].
#[derive(Default)]
pub struct Stats {
    pub checkouts: u64,
    pub waits: u64,
    pub wait_total: Duration,
    pub wait_max: Duration,
    pub in_use_peak: usize,
    pub opened: u64,
    pub evicted: u64,
    /// Handles open when the counters were taken.
    pub open: usize,
}

impl Pool {
    /// A pool whose first idle handle is `ds`, already opened on `path`.
    pub fn new(path: &Path, ds: Dataset, max_open: usize) -> Self {
        Self {
            path: path.to_path_buf(),
            state: Mutex::new(State {
                idle: vec![(ds, Instant::now())],
                open: 1,
                stats: Stats::default(),
            }),
            returned: Condvar::new(),
            max_open: max_open.max(1),
        }
    }

    /// Runs `op` on a handle, waiting while `max_open` are in use.
    pub fn with<T>(&self, op: impl FnOnce(&Dataset) -> Result<T, AppError>) -> Result<T, AppError> {
        let mut guard = Checkout {
            pool: self,
            ds: Some(self.checkout()?),
        };

        let result = op(guard.ds.as_ref().expect("handle checked out"));

        let ds = guard.ds.take().expect("handle checked out");

        self.state
            .lock()
            .expect(POISONED)
            .idle
            .push((ds, Instant::now()));

        self.returned.notify_one();

        result
    }

    fn checkout(&self) -> Result<Dataset, AppError> {
        let mut waiting_since = None;
        let mut state = self.state.lock().expect(POISONED);

        loop {
            // Newest first, so surplus handles stay idle long enough to be evicted.
            if let Some((ds, _)) = state.idle.pop() {
                record_checkout(&mut state, waiting_since);

                return Ok(ds);
            }

            if state.open < self.max_open {
                state.open += 1;
                record_checkout(&mut state, waiting_since);

                break;
            }

            waiting_since.get_or_insert_with(Instant::now);

            state = self.returned.wait(state).expect(POISONED);
        }

        drop(state);

        match Dataset::open(&self.path) {
            Ok(ds) => {
                self.state.lock().expect(POISONED).stats.opened += 1;

                Ok(ds)
            }
            Err(err) => {
                self.state.lock().expect(POISONED).open -= 1;

                self.returned.notify_one();

                Err(err.into())
            }
        }
    }

    /// Closes the handles idle for longer than `after`.
    pub fn evict(&self, after: Duration) {
        let now = Instant::now();

        let expired = {
            let mut state = self.state.lock().expect(POISONED);

            let (keep, expired): (Vec<_>, Vec<_>) = std::mem::take(&mut state.idle)
                .into_iter()
                .partition(|(_, returned_at)| now.duration_since(*returned_at) <= after);

            state.idle = keep;
            state.open -= expired.len();
            state.stats.evicted += expired.len() as u64;

            expired
        };

        // Closed outside the lock.
        drop(expired);
    }

    /// The counters since the previous call, which are reset.
    pub fn take_stats(&self) -> Stats {
        let mut state = self.state.lock().expect(POISONED);

        let in_use = state.open - state.idle.len();
        let mut stats = std::mem::take(&mut state.stats);

        state.stats.in_use_peak = in_use;
        stats.open = state.open;

        stats
    }
}

/// Frees the slot of a handle whose `op` panicked, so waiters are not stranded.
struct Checkout<'a> {
    pool: &'a Pool,
    ds: Option<Dataset>,
}

impl Drop for Checkout<'_> {
    fn drop(&mut self) {
        if let Some(ds) = self.ds.take() {
            drop(ds);

            self.pool.state.lock().expect(POISONED).open -= 1;

            self.pool.returned.notify_one();
        }
    }
}

fn record_checkout(state: &mut State, waiting_since: Option<Instant>) {
    state.stats.checkouts += 1;

    if let Some(since) = waiting_since {
        let waited = since.elapsed();

        state.stats.waits += 1;
        state.stats.wait_total += waited;
        state.stats.wait_max = state.stats.wait_max.max(waited);
    }

    let in_use = state.open - state.idle.len();

    state.stats.in_use_peak = state.stats.in_use_peak.max(in_use);
}
