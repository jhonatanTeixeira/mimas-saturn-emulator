//! Real wall-clock CPU throttling. `LockStepSync` bounds relative drift
//! between cores in abstract cycle-count terms but has no wall-clock
//! reference of its own -- everything here paces execution against actual
//! Saturn hardware clock rates instead, generalizing the same batched-
//! comparison technique `Sh2::run_loop`'s VBLANK pacing and
//! `SaturnSystem::start`'s VDP2 frame pacing already use. See
//! `TECH_DEBT.md` item 4 and `docs/final_architecture_draft.md`'s "CPU
//! clock throttling" section (this module implements that section's
//! pseudocode directly).
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Real Saturn SH-2 clock rate, NTSC 352-dot/28MHz mode (the mode real
/// hardware boots into) -- cross-checked against Yabause's
/// `YabauseChangeTiming()` (`yabause/src/yabause.c:165-167`):
/// `(39_375_000.0 / 11.0) * 8.0` Hz, gated by `CLKTYPE_28MHZ`. Both Master
/// and Slave SH-2 run at this same rate on real hardware. Mimas doesn't
/// implement the SMPC CKCHG352/CKCHG320 commands that would switch modes
/// at runtime, so this is a fixed constant for now, not yet
/// region/mode-aware -- revisit once those commands exist.
pub const SH2_CLOCK_HZ: f64 = (39_375_000.0 / 11.0) * 8.0;

/// Real SCSP onboard M68000 clock rate, same for NTSC/PAL -- cross-checked
/// against Yabause's `scsp2.c:128-129` (`SCSP_CLOCK_FREQ`, explicitly
/// commented "11.2896 MHz"), corroborated independently at
/// `yabause/src/yabause.c:688,694,700`.
pub const M68K_CLOCK_HZ: f64 = 44_100.0 * 256.0;

/// `m68k.rs` doesn't model real per-opcode 68000 cycle costs at all (every
/// `M68k::step()` call executes exactly one instruction with no timing
/// information). Building a real per-opcode M68K cycle table is a separate,
/// much bigger undertaking; charging a flat nominal average keeps it
/// documented as an approximation. See docs/implementation-plans/scsp.md
/// for details on the M68K side.
pub const M68K_NOMINAL_CYCLES_PER_INSTRUCTION: u64 = 8;

/// Real SCSP output sample rate. Derived from `M68K_CLOCK_HZ` (not a second
/// independent literal) since that constant's own citation already
/// establishes it: `SCSP_CLOCK_FREQ` is "11.2896 MHz", and `256` output
/// samples' worth of that clock is exactly `44_100.0 * 256.0` -- i.e. the
/// master clock runs at 256 cycles per output sample, a standard ratio for
/// this class of audio chip. Paces `Scsp::synthesize`'s batches so Core 5
/// doesn't spin flat-out (unless `ThrottleSpeed::Unthrottled`, the default)
/// synthesizing audio far faster than anything could ever play it back.
pub const SCSP_SAMPLE_RATE_HZ: f64 = M68K_CLOCK_HZ / 256.0;

/// Target amount of emulated time per pacing batch. Large enough that OS
/// sleep-precision error (microseconds at best) is negligible relative to
/// it; small enough that frame timing and shutdown responsiveness (which
/// only get rechecked between batches, not mid-batch) stay comfortably
/// finer-grained than a 60Hz frame interval.
const BATCH_DURATION_SECS: f64 = 0.001;

/// Never sleep longer than this in one `advance()` call, regardless of how
/// far the ideal schedule has fallen behind "now" (e.g. a pathologically
/// tiny configured multiplier). `thread::sleep` can't be interrupted once
/// entered, so this is the only lever available to keep a core's
/// `run_loop` responsive to a shutdown signal or a live `set_speed` change
/// -- without it, an extreme slow-motion setting could make
/// `SaturnSystem::shutdown()` hang for however long the in-flight sleep
/// happens to be. For any realistic multiplier the per-batch ideal
/// duration is far under this, and it never actually binds.
const MAX_SINGLE_SLEEP: Duration = Duration::from_millis(50);

/// How fast to run, relative to real hardware. Shared (via `Arc<Mutex<_>>`)
/// between every throttled core and whatever frontend wants to control it
/// live, the same way any other emulator's speed slider works.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ThrottleSpeed {
    /// Run as fast as the host allows -- no pacing at all. The default.
    Unthrottled,
    /// Pace to `multiplier` times real hardware speed (1.0 = real speed).
    /// A non-positive multiplier is treated as `Unthrottled` defensively
    /// (a zero or negative target rate has no sensible batch duration).
    Multiplier(f64),
}

/// Batched wall-clock pacer for one clock domain (one `ClockThrottle` per
/// CPU with its own clock rate -- SH-2 and M68K never share one, since
/// they run at genuinely different real rates). Call `advance()` with the
/// real cycles just executed after every step; it accumulates silently and
/// only actually sleeps once a full batch's worth has built up.
pub struct ClockThrottle {
    clock_hz: f64,
    batch_cycles: u64,
    accumulated: u64,
    speed: Arc<Mutex<ThrottleSpeed>>,
    next_batch_due: Instant,
}

impl ClockThrottle {
    pub fn new(clock_hz: f64, speed: Arc<Mutex<ThrottleSpeed>>) -> Self {
        let batch_cycles = ((clock_hz * BATCH_DURATION_SECS) as u64).max(1);
        Self {
            clock_hz,
            batch_cycles,
            accumulated: 0,
            speed,
            next_batch_due: Instant::now(),
        }
    }

    /// Accumulate `cycles` of real progress. Once a full batch has built
    /// up, paces against the currently configured speed and resets the
    /// accumulator -- a no-op (no lock, no clock read) on every call that
    /// doesn't cross the threshold.
    pub fn advance(&mut self, cycles: u64) {
        self.accumulated += cycles;
        if self.accumulated < self.batch_cycles {
            return;
        }
        let cycles_this_batch = self.accumulated;
        self.accumulated = 0;

        let multiplier = match *self.speed.lock().unwrap() {
            ThrottleSpeed::Unthrottled => None,
            ThrottleSpeed::Multiplier(m) if m > 0.0 => Some(m),
            ThrottleSpeed::Multiplier(_) => None,
        };
        let Some(multiplier) = multiplier else {
            // Unthrottled (or a defensively-rejected non-positive
            // multiplier): stay anchored to "now" so switching back to a
            // real multiplier later doesn't inherit a stale, long-past
            // schedule accumulated from before pacing was disabled.
            self.next_batch_due = Instant::now();
            return;
        };

        // Mirrors `docs/final_architecture_draft.md`'s CPU throttling
        // pseudocode exactly: `next_batch_due` accumulates by a fixed
        // ideal duration each batch (never `now + duration`), so a
        // transient slow batch gets made up by running flat-out for a few
        // subsequent batches once things speed back up, rather than
        // permanently losing that time. A *persistent* inability to keep
        // up degrades to a permanent no-op below (never sleeps, never
        // lies about elapsed time) -- exactly the documented "running
        // behind real-time... consider this observable" behavior.
        let ideal_duration =
            Duration::from_secs_f64(cycles_this_batch as f64 / (self.clock_hz * multiplier));
        self.next_batch_due += ideal_duration;
        let now = Instant::now();
        if self.next_batch_due > now {
            std::thread::sleep((self.next_batch_due - now).min(MAX_SINGLE_SLEEP));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speed(s: ThrottleSpeed) -> Arc<Mutex<ThrottleSpeed>> {
        Arc::new(Mutex::new(s))
    }

    #[test]
    fn unthrottled_never_sleeps() {
        // clock_hz chosen so batch_cycles = 10 -- enough batch completions
        // (1000, over 10,000 calls) to prove repeated non-sleeping, not
        // just a single lucky check.
        let mut throttle = ClockThrottle::new(10_000.0, speed(ThrottleSpeed::Unthrottled));
        let start = Instant::now();
        for _ in 0..10_000 {
            throttle.advance(1);
        }
        // If this were mistakenly paced at Multiplier(1.0) instead, the
        // same workload would take roughly 1 second (1000 batches * 1ms) --
        // 200ms leaves a wide, unambiguous margin either way.
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "Unthrottled must not pace at all"
        );
    }

    #[test]
    fn real_speed_paces_close_to_the_ideal_duration() {
        // Synthetic, deliberately low clock_hz (not the real 28.6MHz) so
        // this test stays fast while still meaningfully timing-sensitive:
        // 10,000 Hz with a 1ms batch = 10 cycles/batch. Ten batches (100
        // cycles) at Multiplier(1.0) should take close to 10ms.
        let clock_hz = 10_000.0;
        let mut throttle = ClockThrottle::new(clock_hz, speed(ThrottleSpeed::Multiplier(1.0)));
        let start = Instant::now();
        for _ in 0..100 {
            throttle.advance(1);
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(7),
            "real-speed throttle paced too fast: {:?}",
            elapsed
        );
        assert!(
            elapsed < Duration::from_millis(300),
            "real-speed throttle paced far too slow: {:?}",
            elapsed
        );
    }

    #[test]
    fn higher_multiplier_paces_faster() {
        let clock_hz = 10_000.0;
        let mut slow = ClockThrottle::new(clock_hz, speed(ThrottleSpeed::Multiplier(1.0)));
        let mut fast = ClockThrottle::new(clock_hz, speed(ThrottleSpeed::Multiplier(50.0)));

        let slow_start = Instant::now();
        for _ in 0..100 {
            slow.advance(1);
        }
        let slow_elapsed = slow_start.elapsed();

        let fast_start = Instant::now();
        for _ in 0..100 {
            fast.advance(1);
        }
        let fast_elapsed = fast_start.elapsed();

        assert!(
            fast_elapsed < slow_elapsed,
            "a higher multiplier must pace faster (real: {:?}, 50x: {:?})",
            slow_elapsed,
            fast_elapsed
        );
    }

    #[test]
    fn advance_never_sleeps_longer_than_the_cap() {
        // An extremely tiny multiplier would ideally sleep for a very long
        // time per batch (here: ~1000s) -- must not actually block that
        // long, or a pathological speed setting could make
        // `SaturnSystem::shutdown()` hang for however long the in-flight
        // sleep happens to be (`thread::sleep` can't be interrupted early).
        let mut throttle = ClockThrottle::new(10_000.0, speed(ThrottleSpeed::Multiplier(0.00001)));
        let start = Instant::now();
        for _ in 0..10 {
            throttle.advance(1); // one full batch (10 cycles)
        }
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "a single advance() call must never block anywhere near the (unbounded) ideal duration: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn non_positive_multiplier_behaves_like_unthrottled() {
        for bad_multiplier in [0.0, -1.0] {
            let mut throttle =
                ClockThrottle::new(10_000.0, speed(ThrottleSpeed::Multiplier(bad_multiplier)));
            let start = Instant::now();
            for _ in 0..10_000 {
                throttle.advance(1);
            }
            assert!(
                start.elapsed() < Duration::from_millis(200),
                "a non-positive multiplier ({}) must not hang or pace",
                bad_multiplier
            );
        }
    }
}
