//! Compiles the flyout `.slint` markup into generated Rust, pulled into the
//! crate by `slint::include_modules!()` in `shell`.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    slint_build::compile("ui/flyout.slint")?;
    Ok(())
}
