use std::sync::{atomic::{AtomicBool, Ordering}, Condvar, Mutex};
use crate::sync::LockStepSync;

pub struct BusArbiter {
    locked_by_dma: AtomicBool,
    unlock_signal: Condvar,
    unlock_mutex: Mutex<()>,
}

impl BusArbiter {
    pub fn new() -> Self {
        Self {
            locked_by_dma: AtomicBool::new(false),
            unlock_signal: Condvar::new(),
            unlock_mutex: Mutex::new(()),
        }
    }
}

impl Default for BusArbiter {
    fn default() -> Self {
        Self::new()
    }
}

impl BusArbiter {
    /// Block the main SH-2 CPUs if DMA is active
    pub fn acquire_bus(&self) {
        if self.locked_by_dma.load(Ordering::Relaxed) {
            let guard = self.unlock_mutex.lock().unwrap();
            let _guard = self.unlock_signal.wait_while(guard, |_| {
                self.locked_by_dma.load(Ordering::Acquire)
            }).unwrap();
        }
    }

    /// Block the main SH-2 CPUs if DMA is active, while updating the sync state to prevent deadlocks
    pub fn acquire_bus_sync(&self, core_id: usize, sync: &LockStepSync) -> Option<u64> {
        if self.locked_by_dma.load(Ordering::Relaxed) {
            sync.set_thread_active(core_id, false);
            {
                let guard = self.unlock_mutex.lock().unwrap();
                let _guard = self.unlock_signal.wait_while(guard, |_| {
                    self.locked_by_dma.load(Ordering::Acquire)
                }).unwrap();
            }
            let caught_up = sync.set_thread_active(core_id, true);
            Some(caught_up)
        } else {
            None
        }
    }

    /// Lock the bus for a DMA transfer
    pub fn lock_for_dma(&self) {
        self.locked_by_dma.store(true, Ordering::Release);
    }

    /// Unlock the bus after DMA completes
    pub fn unlock_from_dma(&self) {
        self.locked_by_dma.store(false, Ordering::Release);
        let _guard = self.unlock_mutex.lock().unwrap();
        self.unlock_signal.notify_all();
    }

    /// Abort any ongoing DMA bus locks and signal blocked threads to wake up
    pub fn abort(&self) {
        self.locked_by_dma.store(false, Ordering::Release);
        let _guard = self.unlock_mutex.lock().unwrap();
        self.unlock_signal.notify_all();
    }

    /// Check if DMA has locked the bus
    pub fn is_locked(&self) -> bool {
        self.locked_by_dma.load(Ordering::Acquire)
    }
}
