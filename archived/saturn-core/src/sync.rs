use std::sync::{Arc, Condvar, Mutex};

pub struct LockStepSync {
    shutdown_flag: std::sync::atomic::AtomicBool,
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
    /// Which cores are currently blocked inside `sync_core`'s drift wait.
    waiting: Vec<bool>,
    /// For each waiter, the minimum active cycle count at which it could
    /// actually proceed (`its own cycles - slack_limit`). Meaningless unless
    /// `waiting[i]`. This is what makes the notify conditional: without it
    /// `sync_core` has no way to tell "the minimum moved" from "the minimum
    /// moved far enough to unblock somebody", and has to broadcast on every
    /// call just in case.
    wake_at: Vec<u64>,
    shutdown: bool,
}

impl LockStepSync {
    pub fn new(num_threads: usize, slack_limit: u64) -> Self {
        if num_threads == 0 || num_threads > 32 {
            panic!("Invalid thread count: {}", num_threads);
        }
        Self {
            shutdown_flag: std::sync::atomic::AtomicBool::new(false),
            num_threads,
            slack_limit,
            state: Mutex::new(SyncState {
                cycles: vec![0; num_threads],
                active: vec![true; num_threads],
                waiting: vec![false; num_threads],
                wake_at: vec![0; num_threads],
                shutdown: false,
            }),
            condvar: Condvar::new(),
            park_condvar: Condvar::new(),
        }
    }

    pub fn slack_limit(&self) -> u64 {
        self.slack_limit
    }

    pub fn sync_core(&self, core_id: usize, current_cycles: u64) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.shutdown {
            return false;
        }

        if core_id >= self.num_threads {
            panic!("Invalid core ID: {}", core_id);
        }

        if !state.active[core_id] {
            return false;
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
                    let diff = current_cycles.wrapping_sub(min_val) as i64;
                    if diff > self.slack_limit as i64 {
                        // Publish what this core is waiting for, so whoever
                        // advances the minimum can tell whether it needs to
                        // wake us at all.
                        state.waiting[core_id] = true;
                        state.wake_at[core_id] = current_cycles.saturating_sub(self.slack_limit);
                        let start = std::time::Instant::now(); // golden-rule-ok: telemetry measurement, not a deadline pacing timer
                        state = self.condvar.wait(state).unwrap();
                        let duration = start.elapsed().as_nanos() as u64;
                        crate::telemetry::record_idle_time(core_id, duration);
                        state.waiting[core_id] = false;
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

        state.waiting[core_id] = false;

        // Conditional broadcast. This used to be an unconditional
        // `notify_all()` on *every* call -- from Core 0 that is once per ~32
        // guest cycles, i.e. ~10^5 futex wakes a second at a core that was
        // Condvar-blocked ~97% of the time and could only make progress every
        // ~650 cycles. Roughly 15 of every 16 wakeups were spurious: the woken
        // core re-locked this same mutex, recomputed the minimum, found nothing
        // had changed for it, and slept again. Now a broadcast only goes out
        // when the minimum active cycle count has actually reached some
        // waiter's threshold.
        //
        // Using the global minimum (rather than each waiter's own "minimum of
        // the others") is deliberately conservative in the safe direction: a
        // waiter is by definition *ahead* of the minimum, so it is never itself
        // the minimum, and `global_min >= wake_at[i]` therefore implies that
        // core's own drift condition is satisfied. Deactivation and shutdown
        // still broadcast unconditionally (`set_thread_active`,
        // `request_shutdown`), which covers the cases where the minimum jumps
        // without anyone calling `sync_core`.
        let mut global_min: Option<u64> = None;
        for i in 0..self.num_threads {
            if state.active[i] {
                global_min = Some(match global_min {
                    None => state.cycles[i],
                    Some(v) => v.min(state.cycles[i]),
                });
            }
        }
        let should_notify = match global_min {
            Some(min_val) => (0..self.num_threads)
                .any(|i| i != core_id && state.waiting[i] && state.wake_at[i] <= min_val),
            None => false,
        };
        if should_notify {
            self.condvar.notify_all();
        }
        true
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
    pub fn get_cycles(&self, core_id: usize) -> u64 {
        let state = self.state.lock().unwrap();
        state.cycles[core_id]
    }

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
        // Mirror into the lock-free flag `is_shutdown()` reads. Without this
        // store the mirror stays false forever and every `is_shutdown()` check
        // in the system is dead code -- including `PanicGuard`'s, whose entire
        // purpose is to stop one core's panic from hanging the others.
        self.shutdown_flag
            .store(true, std::sync::atomic::Ordering::Release);
        self.condvar.notify_all();
        self.park_condvar.notify_all();
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown_flag
            .load(std::sync::atomic::Ordering::Relaxed)
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
