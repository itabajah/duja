//! Compiles the `.slint` markup into generated Rust, pulled into the crate by
//! `slint::include_modules!()` in the `generated` module.
//!
//! Everything is compiled through the single `ui/app.slint` entry, which
//! re-exports the flyout and settings windows; compiling each window separately
//! would re-emit their shared `flyout.slint` imports and collide.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The `smoke` feature enables the `i-slint-backend-testing` interaction
    // tests, whose `ElementHandle` API needs the Slint compiler to emit debug
    // info into the generated code. Only turn it on for that build (it is dead
    // weight otherwise). Cargo exposes an enabled feature as `CARGO_FEATURE_*`.
    if std::env::var_os("CARGO_FEATURE_SMOKE").is_some() {
        let config = slint_build::CompilerConfiguration::new().with_debug_info(true);
        slint_build::compile_with_config("ui/app.slint", config)?;
    } else {
        slint_build::compile("ui/app.slint")?;
    }
    Ok(())
}
