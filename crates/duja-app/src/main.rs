//! Duja tray application entry point.
//!
//! P0 placeholder: prints version and exits. The real assembly (single
//! instance, config, tray, controller thread, `--restore`, `--soak`) lands
//! in P4. `#![windows_subsystem = "windows"]` is added then — keeping a
//! console binary until the tray exists makes the skeleton debuggable.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

fn main() {
    println!("duja {} (pre-alpha scaffold)", duja_core::version());
}
