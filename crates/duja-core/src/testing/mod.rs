//! Test support: deterministic fakes and the reusable controller contract
//! suite, exposed to other crates behind the `test-support` feature.
//!
//! This module is available under `cfg(test)` for this crate's own tests and,
//! for downstream crates, when the `test-support` feature is enabled. It is
//! never compiled into a release build.

pub mod clock;
pub mod contract;
pub mod controller;
pub mod dimmer;

pub use clock::FakeClock;
pub use contract::{Scenario, run_controller_contract};
pub use controller::{Call, FakeController};
pub use dimmer::FakeDimmer;
