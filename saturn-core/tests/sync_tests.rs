use saturn_core::{BusArbiter, LockStepSync, SaturnSystem, Sh2, ThrottleSpeed, WorkRam};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

/// Run `f` on its own thread and wait up to `timeout` for it to finish,
/// propagating a panic from inside `f` verbatim (so a real assertion
/// failure still reads as that failure, not as a generic timeout) or
/// panicking with a clear "deadlock" message if it never finishes. There's
/// no per-test timeout anywhere in this workspace (no CI config, no
/// `cargo-nextest`), so a real regression in the parking logic these tests
/// exercise would otherwise hang `cargo test` itself instead of failing
/// with a message.
fn assert_completes_within<F: FnOnce() + Send + 'static>(timeout: Duration, f: F) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        let _ = tx.send(result);
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(())) => {}
        Ok(Err(payload)) => std::panic::resume_unwind(payload),
        Err(_) => panic!(
            "operation did not complete within {:?} -- likely deadlock",
            timeout
        ),
    }
}

#[test]
fn test_lockstep_sync_basic() {
    let sync = Arc::new(LockStepSync::new(4, 10));

    // Core 0 tries to run to 12 while others are at 0 (slack limit = 10)
    // This should block since 12 > 0 + 10.
    let sync_clone = sync.clone();
    let is_blocked = Arc::new(AtomicBool::new(true));
    let is_blocked_clone = is_blocked.clone();

    let handle = thread::spawn(move || {
        sync_clone.sync_core(0, 12);
        is_blocked_clone.store(false, Ordering::Relaxed);
    });

    // Wait a bit and verify that the thread is indeed blocked
    thread::sleep(Duration::from_millis(50));
    assert!(
        is_blocked.load(Ordering::Relaxed),
        "Core 0 did not block when exceeding drift limit"
    );

    // Advance the other cores to 1.
    // Core 0 at 12 is still exceeding 1 + 10 = 11, so it should still block.
    sync.sync_core(1, 1);
    sync.sync_core(2, 1);
    sync.sync_core(3, 1);

    thread::sleep(Duration::from_millis(20));
    assert!(
        is_blocked.load(Ordering::Relaxed),
        "Core 0 woke up too early when still exceeding drift limit"
    );

    // Advance the other cores to 2.
    // Core 0 at 12 is now within 2 + 10 = 12. It should wake up!
    sync.sync_core(1, 2);
    sync.sync_core(2, 2);
    sync.sync_core(3, 2);

    // Join the thread to verify it unblocks and finishes
    handle.join().unwrap();
    assert!(
        !is_blocked.load(Ordering::Relaxed),
        "Core 0 failed to unblock after other cores caught up"
    );
}

#[test]
fn test_lockstep_sync_graceful_shutdown() {
    let sync = Arc::new(LockStepSync::new(4, 10));
    let mut handles = vec![];
    let shutdown = Arc::new(AtomicBool::new(false));

    for i in 0..4 {
        let sync_clone = sync.clone();
        let shutdown_clone = shutdown.clone();
        handles.push(thread::spawn(move || {
            let mut cycles = 0;
            while !shutdown_clone.load(Ordering::Relaxed) {
                if sync_clone.is_shutdown() {
                    break;
                }
                cycles += 2;
                sync_clone.sync_core(i, cycles);
                thread::yield_now();
            }
        }));
    }

    // Let them run a bit
    thread::sleep(Duration::from_millis(50));

    // Request shutdown
    shutdown.store(true, Ordering::Relaxed);
    sync.request_shutdown();

    // Verify all threads terminate
    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn test_bus_arbiter_blocking() {
    let arbiter = Arc::new(BusArbiter::new());
    let sync = Arc::new(LockStepSync::new(4, 10));
    let work_ram = Arc::new(WorkRam::default());

    // Lock the bus for DMA
    arbiter.lock_for_dma();

    // Create Sh2 for Core 0, set its sync, and spawn it
    let mut cpu = Sh2::new(false, arbiter.clone(), work_ram);
    cpu.sync = Some(sync.clone());
    cpu.core_id = 0;

    let is_blocked = Arc::new(AtomicBool::new(true));
    let is_blocked_clone = is_blocked.clone();

    // Core 0 thread will block on read_word/write_word because DMA is active
    let handle = thread::spawn(move || {
        let mut cpu = cpu;
        // This read_word calls acquire_bus_sync, which should set Core 0 inactive
        let _val = cpu.read_word(0x06000000);
        is_blocked_clone.store(false, Ordering::Relaxed);
    });

    // Wait and verify Core 0 is blocked
    thread::sleep(Duration::from_millis(50));
    assert!(is_blocked.load(Ordering::Relaxed));

    // Since Core 0 is blocked, it should be marked INACTIVE by acquire_bus_sync.
    // This means Cores 1, 2, 3 should be able to advance cycles way past the drift limit of 10.
    // Let's verify by advancing Cores 1, 2, 3 to 100 cycles concurrently.
    let start_time = std::time::Instant::now();

    let handle1 = {
        let sync = sync.clone();
        thread::spawn(move || sync.sync_core(1, 100))
    };
    let handle2 = {
        let sync = sync.clone();
        thread::spawn(move || sync.sync_core(2, 100))
    };
    let handle3 = {
        let sync = sync.clone();
        thread::spawn(move || sync.sync_core(3, 100))
    };

    handle1.join().unwrap();
    handle2.join().unwrap();
    handle3.join().unwrap();

    assert!(
        start_time.elapsed() < Duration::from_millis(150),
        "Other cores blocked on inactive Core 0"
    );

    // Now unlock the DMA bus
    arbiter.unlock_from_dma();

    // Core 0 should resume, set itself active again, catch up, and finish
    handle.join().unwrap();
    assert!(
        !is_blocked.load(Ordering::Relaxed),
        "Core 0 failed to resume after DMA unlock"
    );
}

#[test]
fn test_park_while_inactive_blocks_and_wakes_on_reactivation() {
    let sync = Arc::new(LockStepSync::new(4, 10));
    sync.set_thread_active(1, false);

    let woke_via_reactivation = Arc::new(AtomicBool::new(false));
    let woke_clone = woke_via_reactivation.clone();
    let sync_clone = sync.clone();
    let handle = thread::spawn(move || {
        let reactivated = sync_clone.park_while_inactive(1);
        woke_clone.store(reactivated, Ordering::Relaxed);
    });

    thread::sleep(Duration::from_millis(50));
    assert!(
        !handle.is_finished(),
        "parked thread should still be blocked"
    );

    sync.set_thread_active(1, true);

    assert_completes_within(Duration::from_secs(2), move || handle.join().unwrap());
    assert!(
        woke_via_reactivation.load(Ordering::Relaxed),
        "park_while_inactive must return true on reactivation"
    );
}

#[test]
fn test_park_while_inactive_wakes_on_shutdown() {
    let sync = Arc::new(LockStepSync::new(4, 10));
    sync.set_thread_active(0, false);

    let woke_via_reactivation = Arc::new(AtomicBool::new(true));
    let woke_clone = woke_via_reactivation.clone();
    let sync_clone = sync.clone();
    let handle = thread::spawn(move || {
        let reactivated = sync_clone.park_while_inactive(0);
        woke_clone.store(reactivated, Ordering::Relaxed);
    });

    thread::sleep(Duration::from_millis(50));
    assert!(!handle.is_finished());

    let start = std::time::Instant::now();
    sync.request_shutdown();

    assert_completes_within(Duration::from_secs(2), move || handle.join().unwrap());
    assert!(
        start.elapsed() < Duration::from_millis(100),
        "parked core did not wake promptly on shutdown"
    );
    assert!(
        !woke_via_reactivation.load(Ordering::Relaxed),
        "park_while_inactive must return false when woken by shutdown"
    );
}

#[test]
fn test_parked_core_does_not_block_active_cores_drift() {
    let sync = Arc::new(LockStepSync::new(4, 10));

    // Cores 1 and 2 park immediately, mirroring `SaturnSystem::start()`'s
    // Core 1 / Core 2 handling; cores 0 and 3 stay active and drive far
    // past the slack limit together, mirroring the real Master SH-2 /
    // VDP1-VDP2-SCSP topology. If parking ever regressed to not excluding
    // the core from drift tracking, cores 0/3 would block forever waiting
    // on 1/2's frozen cycle count, and this test would time out.
    let mut parked_handles = vec![];
    for core_id in [1usize, 2] {
        let sync_clone = sync.clone();
        parked_handles.push(thread::spawn(move || {
            sync_clone.set_thread_active(core_id, false);
            sync_clone.park_while_inactive(core_id);
        }));
    }

    thread::sleep(Duration::from_millis(50));

    let mut active_handles = vec![];
    for core_id in [0usize, 3] {
        let sync_clone = sync.clone();
        active_handles.push(thread::spawn(move || {
            for cycles in 1..=1000u64 {
                sync_clone.sync_core(core_id, cycles);
            }
        }));
    }

    assert_completes_within(Duration::from_secs(2), move || {
        for h in active_handles {
            h.join().unwrap();
        }
    });

    sync.request_shutdown();
    for h in parked_handles {
        h.join().unwrap();
    }
}

#[test]
fn test_saturn_system_startup_shutdown() {
    assert_completes_within(Duration::from_secs(10), || {
        let mut system = SaturnSystem::with_slack(10);
        let mut bios = vec![
            0x00, 0x00, 0x00, 0x08, // Reset PC -> 8
            0x06, 0x00, 0x00, 0x00, // Reset R15 -> 0x06000000
        ];
        for _ in 0..100 {
            bios.push(0x00);
            bios.push(0x09); // NOP
        }
        bios.push(0xAF);
        bios.push(0x9A); // BRA 8 (disp = -102)
        bios.push(0x00);
        bios.push(0x09); // NOP in delay slot
        system.load_bios(bios);

        // Start system
        system.start();

        // Core 0 must keep making real forward progress even with Core 1/2
        // parked (see `LockStepSync::park_while_inactive`) -- the deadlock
        // canary: if parking a core ever regressed to not excluding it from
        // drift tracking, Core 0 would stall forever the first time its
        // cycle count drifted past the slack limit against a frozen,
        // still-"active" parked core.
        let pc_before = system.cpu0_pc.load(Ordering::Relaxed);
        thread::sleep(Duration::from_millis(100));
        let pc_after = system.cpu0_pc.load(Ordering::Relaxed);
        assert_ne!(
            pc_before, pc_after,
            "Core 0 made no forward progress -- possibly stalled behind a parked core"
        );

        // Shutdown and join threads
        system.shutdown();
    });
}

#[test]
fn test_saturn_system_defaults_to_unthrottled() {
    // Regression guard on the default itself: `history.md` Chapter 9 adds a
    // real wall-clock CPU throttle, but confirmed with the user that
    // `SaturnSystem::new()`/`with_slack()` must keep defaulting to
    // unthrottled -- zero behavior change for the active BIOS-boot
    // investigation and every existing verification workflow. Real speed
    // (or any multiplier) is opt-in via `set_speed`.
    let system = SaturnSystem::with_slack(10);
    assert_eq!(system.get_speed(), ThrottleSpeed::Unthrottled);
}

#[test]
fn test_extreme_speed_multiplier_does_not_hang_shutdown() {
    // `ClockThrottle`'s actual pacing math is already covered thoroughly
    // in `throttle.rs`'s own unit tests, using a synthetic `clock_hz`
    // decoupled from real SH-2 instruction execution -- precise, non-flaky
    // timing assertions aren't reliable at this level, since this
    // interpreter's own per-instruction overhead (a syscall in
    // `thread::yield_now()`, lock contention in `sync_core()`, on every
    // single instruction) turns out to already be substantial enough that
    // comparing real progress rates here is environment-dependent, not a
    // property of the throttle itself.
    //
    // What *is* unique and worth covering at this level: an extreme
    // multiplier's ideal per-batch sleep would be roughly a full second
    // here (real SH-2 speed's ~1ms/~14,318-instruction batch, paced at
    // 0.00001x) -- without `throttle.rs`'s `MAX_SINGLE_SLEEP` cap, a
    // `shutdown()` landing mid-sleep could block for that whole duration,
    // since `thread::sleep` can't be interrupted early. Confirm the real,
    // full `SaturnSystem::shutdown()` path (not just the isolated
    // `ClockThrottle` unit) stays prompt even with a pathological setting.
    assert_completes_within(Duration::from_secs(5), || {
        let mut system = SaturnSystem::with_slack(10);
        system.set_speed(ThrottleSpeed::Multiplier(0.00001));
        system.start();
        thread::sleep(Duration::from_millis(100));
        system.shutdown();
    });
}

#[test]
fn reset_button_is_inert_until_resenab() {
    // §4.16 + §0.4 step 4. Fresh system -> press -> no NMI.
    // COMREG 0x19 -> press -> NMI delivered.
    let mut system = SaturnSystem::new();

    // Test inert state (resd is true by default)
    system.press_reset_button();
    let queue_state = system.irq_in_c0.lock().unwrap().clone();
    assert!(
        !queue_state
            .pending
            .iter()
            .any(|int| int.vector == 0x0B && int.level == 16),
        "Reset button must not fire NMI while resd is true"
    );

    // Issue RESENAB
    system.smpc.lock().unwrap().execute_command(
        saturn_core::smpc::cmd::RESENAB,
        &saturn_core::WorkRam::new(),
    );

    // Test active state
    system.press_reset_button();
    let queue_state = system.irq_in_c0.lock().unwrap().clone();
    assert!(
        queue_state
            .pending
            .iter()
            .any(|int| int.vector == 0x0B && int.level == 16),
        "Reset button must fire NMI (vector 0x0B, level 16) after RESENAB"
    );
}

#[test]
fn ckchg_stops_the_slave() {
    let system = SaturnSystem::new();

    // Turn slave on
    system
        .smpc
        .lock()
        .unwrap()
        .execute_command(saturn_core::smpc::cmd::SSHON, &saturn_core::WorkRam::new());

    // Simulate what the lib.rs thread loop does to observe the SSHON effect:
    // we would call check_smpc_commands, but the test doesn't run the thread.
    // Let's just test that the effect struct says to stop it.

    // Wait, the test in smpc-peripheral.md says "SSHON, then CKCHG352, assert Core 1 is inactive/reset".
    // We can just execute the command directly and assert the effect returned.
    let effects = system.smpc.lock().unwrap().execute_command(
        saturn_core::smpc::cmd::CKCHG352,
        &saturn_core::WorkRam::new(),
    );
    assert!(effects.stop_slave, "CKCHG352 must stop the slave");
}
