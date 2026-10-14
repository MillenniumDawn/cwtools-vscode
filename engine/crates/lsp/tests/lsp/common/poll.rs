/// Retry a state query until it returns a value or its original deadline
/// expires. Keeping the request and completion condition together avoids
/// open-coded sleeps that can miss a readiness signal or drift past a timeout.
pub fn poll_until<T>(
    deadline: std::time::Instant,
    interval: std::time::Duration,
    mut poll: impl FnMut() -> Option<T>,
) -> Option<T> {
    loop {
        if let Some(value) = poll() {
            return Some(value);
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        std::thread::sleep(interval.min(remaining));
    }
}

/// Retry a state query up to `attempts_limit` times, sleeping `interval`
/// between attempts. Bounded by the attempt count alone, not a wall-clock
/// deadline, so slow responses never reduce the number of observations.
pub fn poll_until_attempts<T>(
    attempts_limit: usize,
    interval: std::time::Duration,
    mut poll: impl FnMut() -> Option<T>,
) -> Option<T> {
    for attempt in 0..attempts_limit {
        if let Some(value) = poll() {
            return Some(value);
        }
        if attempt + 1 < attempts_limit {
            std::thread::sleep(interval);
        }
    }
    None
}
