// A minimal blocking counting semaphore (std has none). Used to cap the total
// number of concurrent SNMP requests all collectors may have in flight toward
// the shared back end (snmpbot or the embedded client), so a burst of poll
// cycles aligning can't collectively overrun it (PERF.md #4).
//
// The internal mutex is held only for the tiny permit-count bookkeeping, never
// across the SNMP call itself, so it is not a contention point of its own.
use std::sync::{Condvar, Mutex};

pub struct Semaphore {
    permits: Mutex<usize>,
    cvar: Condvar,
}

// Releases the permit back to the semaphore on drop.
pub struct SemaphoreGuard<'a> {
    sem: &'a Semaphore,
}

impl Semaphore {
    pub fn new(permits: usize) -> Semaphore {
        Semaphore { permits: Mutex::new(permits), cvar: Condvar::new() }
    }

    // Block until a permit is available, then take it. The returned guard
    // returns the permit when dropped.
    pub fn acquire(&self) -> SemaphoreGuard {
        // Recover the guard on poisoning: the only code under this lock is the
        // permit arithmetic below, so a poisoned lock still holds a valid count.
        let mut permits = self.permits.lock().unwrap_or_else(|e| e.into_inner());
        while *permits == 0 {
            permits = self.cvar.wait(permits).unwrap_or_else(|e| e.into_inner());
        }
        *permits -= 1;
        SemaphoreGuard { sem: self }
    }

    fn release(&self) {
        let mut permits = self.permits.lock().unwrap_or_else(|e| e.into_inner());
        *permits += 1;
        // One permit freed -> wake one waiter.
        self.cvar.notify_one();
    }
}

impl Drop for SemaphoreGuard<'_> {
    fn drop(&mut self) {
        self.sem.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn acquire_release_roundtrip() {
        let sem = Semaphore::new(2);
        let a = sem.acquire();
        let b = sem.acquire();
        drop(a);
        drop(b);
        // Still usable afterwards.
        let _c = sem.acquire();
    }

    #[test]
    fn never_exceeds_permit_count() {
        let sem = Arc::new(Semaphore::new(3));
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..24 {
            let sem = sem.clone();
            let live = live.clone();
            let peak = peak.clone();
            handles.push(std::thread::spawn(move || {
                let _permit = sem.acquire();
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(5));
                live.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert!(peak.load(Ordering::SeqCst) <= 3, "peak {} exceeded 3 permits", peak.load(Ordering::SeqCst));
        assert!(peak.load(Ordering::SeqCst) >= 2, "permits should actually run concurrently");
    }
}
