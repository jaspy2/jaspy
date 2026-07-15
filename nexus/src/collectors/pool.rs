// Bounded fan-out for the per-device poll loops.
//
// The entity and VLAN collectors used to spawn one OS thread per monitored
// device and join them at a barrier — N simultaneous threads each holding a
// blocking HTTP connection to snmpbot. `run_bounded` keeps the same
// items-then-barrier shape but drains the devices through at most
// `max_workers` threads.
//
// Jitter is per *worker*, not per item: each worker sleeps once (uniformly
// within `jitter_msecs`) before draining the queue, which spreads the SNMP
// load at cycle start the way the old per-thread jitter did. Sleeping per
// item inside a worker would instead serialize its share of the queue and
// stretch the cycle wall time by the sum of the draws.
use rand::prelude::*;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::thread;
use std::time;

pub fn run_bounded<T, F>(items: Vec<T>, max_workers: usize, jitter_msecs: u64, work: F)
where
    T: Send,
    F: Fn(T) + Send + Sync,
{
    let workers = std::cmp::min(std::cmp::max(max_workers, 1), items.len());
    let queue: Mutex<VecDeque<T>> = Mutex::new(items.into());
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                if jitter_msecs > 0 {
                    let sleep = thread_rng().gen_range(0.0, jitter_msecs as f64);
                    thread::sleep(time::Duration::from_millis(sleep as u64));
                }
                loop {
                    let item = match queue.lock() {
                        Ok(mut queue) => queue.pop_front(),
                        Err(_) => None,
                    };
                    match item {
                        Some(item) => work(item),
                        None => break,
                    }
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn processes_every_item_exactly_once() {
        let sum = AtomicUsize::new(0);
        run_bounded((1..=100usize).collect(), 4, 0, |item| {
            sum.fetch_add(item, Ordering::Relaxed);
        });
        assert_eq!(sum.load(Ordering::Relaxed), 5050);
    }

    #[test]
    fn concurrency_never_exceeds_the_bound() {
        let live = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        run_bounded((0..64).collect::<Vec<i32>>(), 4, 0, |_| {
            let now = live.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            thread::sleep(time::Duration::from_millis(2));
            live.fetch_sub(1, Ordering::SeqCst);
        });
        let peak = peak.load(Ordering::SeqCst);
        assert!(peak <= 4, "peak concurrency {} exceeded the bound", peak);
        assert!(peak >= 2, "items should actually run concurrently, peak {}", peak);
    }

    #[test]
    fn empty_items_and_zero_workers_return_immediately() {
        run_bounded(Vec::<i32>::new(), 4, 0, |_| panic!("no items to process"));
        // max_workers 0 is clamped to 1 rather than deadlocking.
        let count = AtomicUsize::new(0);
        run_bounded(vec![1, 2, 3], 0, 0, |_| {
            count.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(count.load(Ordering::Relaxed), 3);
    }
}
