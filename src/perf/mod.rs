//! Performance & analysis subsystem: startup timing, memory measurement, and perf helpers.
//!
//! Three sub-modules:
//! - [`memory`]  — platform RSS measurement, sampled at most every 2 seconds.
//! - [`startup`] — instrumented startup timer, history persistence, and breakdown panel.

pub mod memory;
pub mod startup;

pub use memory::get_rss_bytes;
pub use startup::{StartupData, StartupEvent, StartupHistoryEntry, StartupTimer};
