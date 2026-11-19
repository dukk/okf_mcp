//! The bounded caches (milestone 18). Every cache is built on
//! [`bounded::Bounded`]: a size-limited async cache with single-flight
//! loads, per-key invalidation epochs and stale marking.

pub mod bounded;
