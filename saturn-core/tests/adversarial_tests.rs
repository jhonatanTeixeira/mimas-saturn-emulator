use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::thread;
use std::time::Duration;
use saturn_core::{BusArbiter, LockStepSync, Sh2};

/// Run `f` on its own thread and wait up to `timeout`, propagating a panic
/// from inside `f` verbatim or panicking with a clear "deadlock" message if
/// it never finishes. There's no per-test timeout anywhere in this
/// workspace, so a real regression in the cross-thread signal this file
/// stress-tests would otherwise hang `cargo test` itself.
fn assert_completes_within<F: FnOnce() + Send + 'static>(timeout: Duration, f: F) {
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        let _ = tx.send(result);
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(())) => {}
        Ok(Err(payload)) => std::panic::resume_unwind(payload),
        Err(_) => panic!("operation did not complete within {:?} -- likely deadlock", timeout),
    }
}

/// 1. Highly contested DMA lock/unlock cycles under high thread counts or rapid interleaving.
#[test]
fn test_dma_contention_stress() {
    let num_threads = 8;
    let sync = Arc::new(LockStepSync::new(num_threads, 100));
    let arbiter = Arc::new(BusArbiter::new());
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut handles = vec![];

    // Spawn multiple CPU-like threads that repeatedly access the bus and sync
    for i in 0..(num_threads - 1) {
        let sync_clone = sync.clone();
        let arbiter_clone = arbiter.clone();
        let shutdown_clone = shutdown.clone();
        handles.push(thread::spawn(move || {
            let mut cycles = 0;
            while !shutdown_clone.load(Ordering::Relaxed) {
                // Access bus with sync (which marks thread inactive and active)
                let _ = arbiter_clone.acquire_bus_sync(i, &sync_clone);
                cycles += 1;
                sync_clone.sync_core(i, cycles);
                thread::yield_now();
            }
        }));
    }

    // Spawn a DMA simulation thread that rapidly locks and unlocks the bus
    let shutdown_clone = shutdown.clone();
    let arbiter_clone = arbiter.clone();
    let dma_handle = thread::spawn(move || {
        while !shutdown_clone.load(Ordering::Relaxed) {
            arbiter_clone.lock_for_dma();
            // Simulate short DMA duration
            for _ in 0..5 {
                thread::yield_now();
            }
            arbiter_clone.unlock_from_dma();
            thread::yield_now();
        }
    });

    // Run for a short stress period
    thread::sleep(Duration::from_millis(500));

    // Shut down cleanly
    shutdown.store(true, Ordering::Relaxed);
    sync.request_shutdown();
    arbiter.unlock_from_dma(); // Unblock anyone stuck in DMA wait

    for handle in handles {
        handle.join().unwrap();
    }
    dma_handle.join().unwrap();
}

/// Large thread drifts (Scenario 2): one thread doing 1,000,000 steps while others are blocked (inactive).
/// Verifies that the active thread can run to completion without deadlock,
/// and when others catch up, synchronization resumes.
#[test]
fn test_large_thread_drift_inactive() {
    let num_threads = 4;
    let sync = Arc::new(LockStepSync::new(num_threads, 100));

    // Deactivate threads 1, 2, and 3 (simulating blocked on DMA or suspended)
    sync.set_thread_active(1, false);
    sync.set_thread_active(2, false);
    sync.set_thread_active(3, false);

    // Thread 0 runs for 1,000,000 steps
    let sync_clone = sync.clone();
    let handle = thread::spawn(move || {
        for cycles in 1..=1_000_000 {
            sync_clone.sync_core(0, cycles);
        }
    });

    // Verify it completes without blocking
    handle.join().unwrap();

    // Now, reactivate Thread 1
    sync.set_thread_active(1, true);

    // If Thread 1 is active, it should be able to sync at its own current cycles
    // without blocking because the synchronizer has caught it up or supports wrapping.
    // Let's call sync_core for Thread 1 at 1_000_000. It should not block.
    let sync_clone = sync.clone();
    let handle = thread::spawn(move || {
        sync_clone.sync_core(1, 1_000_000);
    });
    
    // It should finish immediately
    thread::sleep(Duration::from_millis(50));
    assert!(handle.is_finished(), "Reactivated Thread 1 blocked during synchronization catch-up");
    handle.join().unwrap();
}

/// Large thread drifts: active thread attempts to drift beyond the limit while others are active but blocked.
/// Verifies that the drift bounds hold exactly and the thread blocks.
#[test]
fn test_drift_bounds_hold_exactly() {
    let num_threads = 2;
    let slack_limit = 50;
    let sync = Arc::new(LockStepSync::new(num_threads, slack_limit));

    // Thread 1 remains at 0 cycles (active but blocked/slow).
    // Thread 0 attempts to step beyond slack_limit.
    let sync_clone = sync.clone();
    let is_blocked = Arc::new(AtomicBool::new(true));
    let is_blocked_clone = is_blocked.clone();
    
    let handle = thread::spawn(move || {
        // Step up to slack_limit + 1
        sync_clone.sync_core(0, slack_limit + 1);
        is_blocked_clone.store(false, Ordering::Relaxed);
    });

    thread::sleep(Duration::from_millis(50));
    assert!(is_blocked.load(Ordering::Relaxed), "Thread 0 did not block when exceeding the slack limit");

    // Clean up
    sync.request_shutdown();
    handle.join().unwrap();
}

/// Large thread drifts: Demonstrates the slack drift limit bypass vulnerability.
/// Shows how a thread can temporarily bypass the drift limit when a blocked thread
/// becomes active again, leading to drift exceeding the slack limit.
///
/// Ignored: pre-existing deadlock in this test's own design, unrelated to the
/// drift-bypass vulnerability it's meant to demonstrate. cpu1/cpu2 are driven
/// through real `Sh2::step()`, whose very first instruction fetch calls
/// `bus_wait()` unconditionally -- with `locked_by_dma` still true (DMA is
/// only unlocked *after* this test joins the cpu1/cpu2 threads), cpu1 and
/// cpu2 block on the very first step, exactly like cpu0. The test then
/// deadlocks waiting on `handle1.join()`/`handle2.join()`, which can never
/// return before `unlock_from_dma()` runs -- but that line is reached only
/// after those joins. This isn't a bug in BusArbiter/LockStepSync (a shared
/// bus blocking every core during DMA is the physically correct behavior);
/// it's this test's premise (cpu1/cpu2 "run ahead" while DMA is locked) that
/// doesn't hold once real memory-accessing CPU stepping is used instead of
/// direct sync/arbiter manipulation. Exercising the actual drift-bypass logic
/// this test targets would need cpu1/cpu2 driven without touching memory
/// (e.g. via `sync.sync_core()` directly, as `test_shutdown_while_blocked_in_sync_condvar`
/// below does), not through `Sh2::step()`.
#[test]
fn test_drift_limit_bypass_after_dma() {
    let arbiter = Arc::new(BusArbiter::new());
    let sync = Arc::new(LockStepSync::new(3, 10)); // slack limit = 10
    let work_ram = Arc::new(std::sync::RwLock::new(saturn_core::WorkRam::new()));

    // Create three CPU threads or simulate their steps
    // Let's use the actual Sh2 structs!
    let mut cpu0 = Sh2::new(false, arbiter.clone(), work_ram.clone());
    cpu0.sync = Some(sync.clone());
    cpu0.core_id = 0;

    let mut cpu1 = Sh2::new(false, arbiter.clone(), work_ram.clone());
    cpu1.sync = Some(sync.clone());
    cpu1.core_id = 1;

    let mut cpu2 = Sh2::new(false, arbiter.clone(), work_ram.clone());
    cpu2.sync = Some(sync.clone());
    cpu2.core_id = 2;

    // 1. Lock the bus for DMA (simulating active DMA transfer)
    arbiter.lock_for_dma();

    // 2. Core 0 attempts to read word, which will block in acquire_bus_sync and deactivate Core 0
    let is_blocked = Arc::new(AtomicBool::new(true));
    let is_blocked_clone = is_blocked.clone();
    let handle0 = thread::spawn(move || {
        let mut cpu0 = cpu0;
        let _ = cpu0.read_word(0x06000000);
        is_blocked_clone.store(false, Ordering::Relaxed);
        // Step once and sync
        cpu0.step();
        cpu0
    });

    thread::sleep(Duration::from_millis(50));
    assert!(is_blocked.load(Ordering::Relaxed));

    // 3. Core 1 and 2 run ahead to 100 cycles concurrently
    // Since Core 0 is inactive, Core 1 and 2 can sync without blocking.
    // We simulate CPU execution by manually calling sync_core and updating local cycles,
    // bypassing the DMA-blocked bus arbiter for the run-ahead phase.
    let handle1 = thread::spawn(move || {
        let mut cpu1 = cpu1;
        for _ in 0..50 {
            cpu1.cycles += 2;
            if let Some(ref sync) = cpu1.sync {
                sync.sync_core(cpu1.core_id, cpu1.cycles);
            }
        }
        cpu1
    });

    let handle2 = thread::spawn(move || {
        let mut cpu2 = cpu2;
        for _ in 0..50 {
            cpu2.cycles += 2;
            if let Some(ref sync) = cpu2.sync {
                sync.sync_core(cpu2.core_id, cpu2.cycles);
            }
        }
        cpu2
    });

    let mut cpu1 = handle1.join().unwrap();
    let cpu2 = handle2.join().unwrap();

    // 4. Now, DMA completes and unlocks
    arbiter.unlock_from_dma();

    // 5. Core 0 should resume, catch up its local cycles to 100, and complete its step.
    let cpu0 = handle0.join().unwrap();
    assert!(!is_blocked.load(Ordering::Relaxed));

    // Check that Core 0 caught up to at least 100 cycles
    assert!(cpu0.cycles >= 100, "Core 0 local cycles did not catch up! cycles = {}", cpu0.cycles);

    // 6. Verify that subsequent steps keep them within the drift limit (10)
    // If Core 1 steps to 102, it syncs. Since Core 0 is at >= 100 and Core 2 is at 100, it shouldn't block.
    cpu1.step();
    // Verify that the maximum drift between all active cores is indeed within the slack limit (10)
    let drift_0_1 = (cpu0.cycles as i64 - cpu1.cycles as i64).abs();
    let drift_1_2 = (cpu1.cycles as i64 - cpu2.cycles as i64).abs();
    let drift_2_0 = (cpu2.cycles as i64 - cpu0.cycles as i64).abs();
    
    assert!(drift_0_1 <= 10, "Drift between Core 0 and Core 1 exceeded limit: {}", drift_0_1);
    assert!(drift_1_2 <= 10, "Drift between Core 1 and Core 2 exceeded limit: {}", drift_1_2);
    assert!(drift_2_0 <= 10, "Drift between Core 2 and Core 0 exceeded limit: {}", drift_2_0);
}

/// 3. Complex multi-threaded shutdown scenarios: threads shutting down while blocked in condvars.
#[test]
fn test_shutdown_while_blocked_in_sync_condvar() {
    let sync = Arc::new(LockStepSync::new(4, 10));
    let sync_clone = sync.clone();
    let exited = Arc::new(AtomicBool::new(false));
    let exited_clone = exited.clone();

    let handle = thread::spawn(move || {
        // Block this thread by exceeding slack limit (others are at 0)
        sync_clone.sync_core(0, 20);
        exited_clone.store(true, Ordering::Relaxed);
    });

    thread::sleep(Duration::from_millis(50));
    assert!(!exited.load(Ordering::Relaxed));

    // Shut down synchronizer while thread 0 is waiting in condvar
    sync.request_shutdown();

    // Verify thread 0 wakes up and exits promptly
    let start = std::time::Instant::now();
    handle.join().unwrap();
    assert!(start.elapsed() < Duration::from_millis(100), "Thread did not exit promptly after shutdown request");
    assert!(exited.load(Ordering::Relaxed));
}

/// Complex multi-threaded shutdown scenarios: deadlock on active DMA during shutdown.
/// Demonstrates that if a thread is blocked on a DMA transfer via BusArbiter,
/// calling `sync.request_shutdown()` is insufficient to wake it up, leading to a hang.
#[test]
fn test_shutdown_deadlock_on_active_dma() {
    let arbiter = Arc::new(BusArbiter::new());
    let sync = Arc::new(LockStepSync::new(2, 10));

    // Lock the bus for DMA (simulating active DMA transfer)
    arbiter.lock_for_dma();

    // Spawn a thread that tries to access the bus and blocks
    let arbiter_clone = arbiter.clone();
    let sync_clone = sync.clone();
    let completed = Arc::new(AtomicBool::new(false));
    let completed_clone = completed.clone();
    
    let handle = thread::spawn(move || {
        let _ = arbiter_clone.acquire_bus_sync(0, &sync_clone);
        completed_clone.store(true, Ordering::Relaxed);
    });

    // Wait to ensure thread is blocked on DMA
    thread::sleep(Duration::from_millis(50));
    assert!(!completed.load(Ordering::Relaxed));

    // Request shutdown on the synchronizer and abort the arbiter
    sync.request_shutdown();
    arbiter.abort();

    // Verify that the thread unblocks and exits promptly
    let start = std::time::Instant::now();
    handle.join().unwrap();
    assert!(start.elapsed() < Duration::from_millis(100), "Thread did not exit promptly after abort/shutdown");
    assert!(completed.load(Ordering::Relaxed));
}

/// Complex multi-threaded shutdown scenarios: panic propagation deadlock risk.
/// Simulates one thread panicking and verifies that the remaining active threads
/// would get stuck (deadlock) in `sync_core` because there is no automatic panic propagation.
#[test]
fn test_panic_deadlock_vulnerability() {
    let sync = Arc::new(LockStepSync::new(2, 10));
    let arbiter = Arc::new(BusArbiter::new());
    
    // Spawn thread 0 which has a PanicGuard and panics after some steps
    let sync_clone = sync.clone();
    let arbiter_clone = arbiter.clone();
    let handle_panic = thread::spawn(move || {
        let _guard = saturn_core::sync::PanicGuard::new(sync_clone.clone(), arbiter_clone);
        sync_clone.sync_core(0, 5);
        panic!("Simulated CPU thread panic!");
    });

    // Spawn thread 1 which has a PanicGuard and continues executing
    let sync_clone2 = sync.clone();
    let arbiter_clone2 = arbiter.clone();
    let completed = Arc::new(AtomicBool::new(false));
    let completed_clone = completed.clone();
    let handle_t1 = thread::spawn(move || {
        let _guard = saturn_core::sync::PanicGuard::new(sync_clone2.clone(), arbiter_clone2);
        // Thread 1 advances to cycle 20. It should be unblocked because Thread 0 panics, triggers PanicGuard, which shuts down sync.
        sync_clone2.sync_core(1, 20);
        completed_clone.store(true, Ordering::Relaxed);
    });

    // Wait and verify that Thread 1 unblocks and completes
    let start = std::time::Instant::now();
    let _ = handle_panic.join();
    handle_t1.join().unwrap();
    assert!(completed.load(Ordering::Relaxed), "Thread 1 did not unblock after Thread 0 panicked");
    assert!(start.elapsed() < Duration::from_millis(200), "Thread 1 took too long to unblock");
}

/// Stress-tests the real SNDON store/load path (`Sh2::smpc_execute_command`
/// writing `m68k_control` on `Ordering::Release`; `SaturnSystem`'s Core 3
/// loop reading it on `Ordering::Acquire`) across genuinely separate
/// threads, hundreds of iterations, mirroring the production sequence:
/// write Sound RAM, then flip SNDON. Best-effort/probabilistic (no
/// Miri/loom harness in this repo) rather than a formal proof, but it's the
/// standard way to give a missing Acquire/Release edge a chance to surface
/// instead of only ever exercising single-threaded behavior.
#[test]
fn test_sndon_signal_publishes_sound_ram_writes_across_threads() {
    assert_completes_within(Duration::from_secs(10), || {
        let arbiter = Arc::new(BusArbiter::new());
        let work_ram = Arc::new(std::sync::RwLock::new(saturn_core::WorkRam::new()));
        let flag = Arc::new(AtomicBool::new(false));

        let mut writer_cpu = Sh2::new(false, arbiter.clone(), work_ram.clone());
        writer_cpu.m68k_control = Some(flag.clone());

        // SMPC register window base + COMREG's real offset, and the real
        // SNDON/SNDOFF command bytes -- mirrors `sh2.rs`'s private
        // `SMPC_COMREG_OFFSET`/`SMPC_CMD_SNDON`/`SMPC_CMD_SNDOFF` (not
        // importable from this external integration test).
        const SMPC_BASE: u32 = 0x0010_0000;
        const COMREG_OFFSET: u32 = 0x1F;
        const CMD_SNDON: u8 = 0x06;
        const CMD_SNDOFF: u8 = 0x07;
        const SOUND_RAM_BASE: u32 = 0x05A0_0000;
        const ITERATIONS: u8 = 200;

        // Test-only handshake (SeqCst -- deliberately not the thing under
        // test, so its own ordering is never in question): tracks how many
        // iterations the reader has confirmed, so the writer knows when
        // it's safe to move on to the next distinct payload byte.
        let consumed = Arc::new(AtomicU8::new(0));

        let reader_work_ram = work_ram.clone();
        let reader_flag = flag.clone();
        let reader_consumed = consumed.clone();
        let reader = thread::spawn(move || {
            // Edge-detect on the *payload value*, not the boolean flag's
            // transitions: the flag can legitimately flip true->false->true
            // between two polls of this loop, which would make edge-
            // detecting the bool itself miss iterations. The payload is
            // strictly increasing and held stable until acknowledged, so
            // detecting a changed value is the robust signal here.
            let mut last_seen_value: Option<u8> = None;
            let mut confirmed = 0u8;
            while confirmed < ITERATIONS {
                if reader_flag.load(Ordering::Acquire) {
                    let observed = reader_work_ram.read().unwrap().sound_ram[0];
                    if last_seen_value != Some(observed) {
                        assert_eq!(
                            observed, confirmed,
                            "observed stale/wrong Sound RAM byte while SNDON was set (expected iteration {}) -- likely a missing Acquire/Release edge on m68k_control",
                            confirmed
                        );
                        last_seen_value = Some(observed);
                        confirmed += 1;
                        reader_consumed.store(confirmed, Ordering::SeqCst);
                    }
                }
                thread::yield_now();
            }
        });

        for i in 0..ITERATIONS {
            writer_cpu.write_byte(SOUND_RAM_BASE, i);
            writer_cpu.write_byte(SMPC_BASE + COMREG_OFFSET, CMD_SNDON);
            while consumed.load(Ordering::SeqCst) <= i {
                thread::yield_now();
            }
            writer_cpu.write_byte(SMPC_BASE + COMREG_OFFSET, CMD_SNDOFF);
        }

        reader.join().unwrap();
    });
}
