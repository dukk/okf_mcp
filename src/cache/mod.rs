//! The bounded caches (milestone 18). Every cache is built on
//! [`bounded::Bounded`]: a size-limited async cache with single-flight
//! loads, per-key invalidation epochs and stale marking.

pub mod bounded;
pub mod credentials;
pub mod directory;
pub mod labels;

pub use bounded::{Bounded, CacheName, Limit};

#[cfg(test)]
mod tests {
    // The names later tasks import: `cache::{Bounded, CacheName, Limit}`.
    use std::time::Duration;

    use super::{Bounded, CacheName, Limit};

    #[test]
    fn the_cache_names_are_reachable_from_the_cache_module() {
        let cache = Bounded::<i64, i64>::new(
            CacheName::Directory,
            Limit::Entries(1),
            Duration::from_secs(1),
        );
        assert!(format!("{cache:?}").contains("Directory"));
    }
}
