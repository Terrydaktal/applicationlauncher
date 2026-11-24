//! Unique product transaction IDs, independent of optional diagnostic state.
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn next_id() -> u64 {
    SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1
}

#[cfg(test)]
mod tests {
    #[test]
    fn product_identifiers_increase_without_initializing_diagnostics() {
        let first = super::next_id();
        assert!(super::next_id() > first);
    }
}
