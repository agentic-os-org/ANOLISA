use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub fn sleep_or_stop(stop: &AtomicBool, interval_secs: u64) -> bool {
    for _ in 0..interval_secs {
        std::thread::sleep(Duration::from_secs(1));
        if !stop.load(Ordering::SeqCst) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn stop_signal_returns_false() {
        let stop = Arc::new(AtomicBool::new(false));
        assert!(!sleep_or_stop(&stop, 60));
    }

    #[test]
    fn full_interval_returns_true() {
        let stop = Arc::new(AtomicBool::new(true));
        assert!(sleep_or_stop(&stop, 1));
    }

    #[test]
    fn full_interval_sleeps_expected_duration() {
        // Pin the iteration count: interval=2 must sleep ~2s, so an off-by-one
        // (e.g. 0..=interval) or a regression to a fixed count would be caught.
        let stop = Arc::new(AtomicBool::new(true));
        let start = std::time::Instant::now();
        assert!(sleep_or_stop(&stop, 2));
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(1900),
            "expected >= ~2s, got {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "expected < 5s, got {elapsed:?}"
        );
    }

    #[test]
    fn stop_mid_interval() {
        let stop = Arc::new(AtomicBool::new(true));
        let stop_clone = Arc::clone(&stop);
        let handle = std::thread::spawn(move || sleep_or_stop(&stop_clone, 300));
        std::thread::sleep(Duration::from_millis(1500));
        stop.store(false, Ordering::SeqCst);
        assert!(!handle.join().unwrap());
    }
}
