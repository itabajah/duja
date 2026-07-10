//! Compiles the `.slint` markup into generated Rust, pulled into the crate by
//! `slint::include_modules!()` in the `generated` module.
//!
//! Everything is compiled through the single `ui/app.slint` entry, which
//! re-exports the flyout and settings windows; compiling each window separately
//! would re-emit their shared `flyout.slint` imports and collide.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    slint_build::compile("ui/app.slint")?;
    Ok(())
}
