//! The Slint-generated component code, quarantined behind blanket lint allows.
//!
//! `slint-build` emits machine-generated Rust (vtables, item trees, unwraps on
//! invariants the runtime upholds) that cannot satisfy this workspace's lint
//! wall. Confining `include_modules!` to this one module — with the wall's lints
//! allowed *only here* — keeps every hand-written line under the full wall.
//!
//! Both the flyout ([`shell`](crate::shell)) and the settings window
//! ([`settings_shell`](crate::settings_shell)) import their components from here,
//! so the generated code is included exactly once for the whole crate.

// RATIONALE: generated code is not ours to fix; the allows are scoped to this
// module and never leak to the shells' own logic.
#![allow(clippy::all, clippy::pedantic)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::undocumented_unsafe_blocks
)]
#![allow(
    dead_code,
    unused,
    non_camel_case_types,
    non_snake_case,
    unsafe_op_in_unsafe_fn,
    unsafe_code
)]
#![allow(rust_2018_idioms)]

slint::include_modules!();
