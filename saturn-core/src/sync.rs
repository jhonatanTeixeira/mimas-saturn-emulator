use std::sync::{Mutex, Condvar, Arc};

pub struct LockStepSync {
    num_threads: usize,
    slack_limit: u64,
    state: Mutex<SyncState>,
    /// Drift-sync waiters (`sync_core`). Notified on *every* call, up to
    /// once per emulated instruction from an active core -- deliberately
    /// separate from `park_condvar` below, which must stay quiet except on
    /// a genuine reactivation/shutdown. Sharing one condvar between both
    /// purposes was tried first and measured as a real bug: a parked core
    /// (see `park_while_inactive`) got spuriously woken on every single
    /// `sync_core` call from every active core, re-contending for this same
    /// mutex millions of times a second for nothing -- parking never
    /// actually reached zero CPU in a running system. See `history.md`.
    condvar: Condvar,
    /// `park_while_inactive` waiters only. Notified solely by
    /// `set_thread_active`'s reactivation branch and `request_shutdown` --
    /// nothing else touches it, so a parked core is only ever woken for a
    /// real reason.
    park_condvar: Condvar,
}

struct SyncState {
    cycles: Vec<u64>,
    active: Vec<bool>,
    shutdown: bool,
}

impl LockStepSync {
    pub fn new(num_threads: usize, slack_limit: u64) -> Self {
        if num_threads == 0 || num_threads > 32 {
            panic!("Invalid thread count: {}", num_threads);
        }
        Self {
            num_threads,
            slack_limit,
            state: Mutex::new(SyncState {
                cycles: vec![0; num_threads],
                active: vec![true; num_threads],
                shutdown: false,
            }),
            condvar: Condvar::new(),
            park_condvar: Condvar::new(),
        }
    }

    pub fn sync_core(&self, core_id: usize, current_cycles: u64) {
        let mut state = self.state.lock().unwrap();
        if state.shutdown {
            return;
        }

        if core_id >= self.num_threads {
            panic!("Invalid core ID: {}", core_id);
        }

        state.cycles[core_id] = current_cycles;

        while !state.shutdown {
            // Find the minimum cycles among other active threads
            let mut min_others = None;
            for i in 0..self.num_threads {
                if i != core_id && state.active[i] {
                    match min_others {
                        None => min_others = Some(state.cycles[i]),
                        Some(val) => min_others = Some(val.min(state.cycles[i])),
                    }
                }
            }

            match min_others {
                Some(min_val) => {
                    // Calculate wrapping difference as signed distance
                    let diff = current_cycles.wrapping_sub(min_val) as i64;
                    if diff > self.slack_limit as i64 {
                        // Drift exceeded, wait for slower active cores to catch up
                        state = self.condvar.wait(state).unwrap();
                    } else {
                        break;
                    }
                }
                None => {
                    // No other active threads, no synchronization needed
                    break;
                }
            }
        }

        // Notify other threads because we have updated cycles or woke up
        self.condvar.notify_all();
    }

    pub fn set_thread_active(&self, core_id: usize, active: bool) -> u64 {
        let mut state = self.state.lock().unwrap();
        if core_id >= self.num_threads {
            panic!("Invalid core ID: {}", core_id);
        }

        if state.active[core_id] && !active {
            state.active[core_id] = false;
            // Notify threads waiting in sync_core, since excluding this core
            // might advance the minimum active cycle.
            self.condvar.notify_all();
        } else if !state.active[core_id] && active {
            // Catch up cycles to the minimum of active threads upon resumption
            let mut min_active = None;
            for i in 0..self.num_threads {
                if state.active[i] {
                    match min_active {
                        None => min_active = Some(state.cycles[i]),
                        Some(val) => min_active = Some(val.min(state.cycles[i])),
                    }
                }
            }
            if let Some(min_val) = min_active {
                state.cycles[core_id] = min_val;
            }
            state.active[core_id] = true;
            // Notify threads in case we changed state
            self.condvar.notify_all();
            // Also wake any `park_while_inactive` waiter for this exact
            // reactivation -- see `park_condvar`'s doc comment for why this
            // is a separate condvar from the one just above.
            self.park_condvar.notify_all();
        }
        state.cycles[core_id]
    }

    /// Block at zero CPU while this core is inactive (see
    /// `set_thread_active`), waking the instant it's reactivated or
    /// shutdown is requested. Mirrors `BusArbiter::acquire_bus`'s
    /// Condvar-park shape, reusing this struct's own `Mutex`+`Condvar`
    /// rather than a second one -- for components that currently have
    /// nothing real to do (see `SaturnSystem::start`'s Core 1/Core 2).
    /// Returns `false` if shutdown fired before this core was ever
    /// reactivated; the caller should return immediately without doing any
    /// work in that case. A future reactivation calls the existing
    /// `set_thread_active(core_id, true)` -- no separate "enable" API,
    /// since that call already seeds this core's cycle count to the
    /// current active minimum, which a bespoke enable flag would skip.
    pub fn park_while_inactive(&self, core_id: usize) -> bool {
        let mut state = self.state.lock().unwrap();
        if core_id >= self.num_threads {
            panic!("Invalid core ID: {}", core_id);
        }
        while !state.active[core_id] && !state.shutdown {
            state = self.park_condvar.wait(state).unwrap();
        }
        !state.shutdown
    }

    pub fn request_shutdown(&self) {
        let mut state = self.state.lock().unwrap();
        state.shutdown = true;
        self.condvar.notify_all();
        self.park_condvar.notify_all();
    }

    pub fn is_shutdown(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.shutdown
    }
}

pub struct PanicGuard {
    sync: Arc<LockStepSync>,
    arbiter: Arc<crate::bus_arbiter::BusArbiter>,
}

impl PanicGuard {
    pub fn new(sync: Arc<LockStepSync>, arbiter: Arc<crate::bus_arbiter::BusArbiter>) -> Self {
        Self { sync, arbiter }
    }
}

impl Drop for PanicGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.sync.request_shutdown();
            self.arbiter.abort();
        }
    }
}
