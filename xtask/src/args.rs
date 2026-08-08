//! The argument-parsing pieces both subcommands need.
//!
//! One rule lives here rather than in either caller, because it is a *decision*
//! about what a command line means and the two subcommands had drifted apart on
//! it: `dist` refused a flag-shaped value and `size` accepted one, so
//! `xtask size --target --release` looked under `target/--release/release` and
//! then reported a missing binary at a path nobody typed.

/// The value following `flag`.
///
/// # Errors
/// Rejects a missing value, and one that is itself flag-shaped: `--version
/// --target macos` would otherwise parse as the *version* `--target` — every
/// character in it is in `dist`'s version alphabet — and then fail with a
/// message about the wrong argument entirely.
pub(crate) fn value<I: Iterator<Item = String>>(
    args: &mut I,
    flag: &str,
) -> Result<String, String> {
    match args.next() {
        Some(v) if !v.starts_with("--") => Ok(v),
        Some(v) => Err(format!("`{flag}` needs a value, but got the flag `{v}`")),
        None => Err(format!("`{flag}` needs a value")),
    }
}

#[cfg(test)]
mod tests {
    use super::value;

    /// Drive [`value`] the way a subcommand's loop does: the flag has already
    /// been consumed, so what it sees is the rest of the line.
    fn after(flag: &str, rest: &[&str]) -> Result<String, String> {
        let mut args = rest.iter().map(|s| (*s).to_owned());
        value(&mut args, flag)
    }

    #[test]
    fn an_ordinary_value_is_taken() {
        assert_eq!(
            after("--target", &["x86_64-pc-windows-msvc"]).as_deref(),
            Ok("x86_64-pc-windows-msvc")
        );
    }

    #[test]
    fn a_missing_value_names_the_flag_that_wanted_one() {
        let err = after("--target", &[]).expect_err("nothing follows the flag");
        assert!(err.contains("--target"), "{err}");
    }

    /// The rule this module exists for. Without it the *next flag* becomes the
    /// value and the command fails somewhere else entirely, describing an
    /// argument the user did not type.
    #[test]
    fn a_flag_shaped_value_is_refused_rather_than_consumed() {
        let err = after("--version", &["--target", "macos"]).expect_err("a flag is not a value");
        assert!(err.contains("--version"), "{err}");
        assert!(
            err.contains("--target"),
            "the message names what it found: {err}"
        );
    }

    /// The boundary of the rule: **two** dashes make a flag, one does not.
    ///
    /// `-` is `dist`'s ad-hoc `codesign` identity, and two caveats are owed
    /// rather than left for a reader to discover. It is a *default* rather than
    /// something users type - `--sign` omitted yields `AD_HOC` - and `dist
    /// --sign -` is legal only when the target is macOS, since `--sign` on any
    /// other target is refused outright as a flag that target cannot honour.
    ///
    /// What makes the case worth pinning is neither of those. It is that
    /// `starts_with('-')` is the obvious tightening of this rule, and it would
    /// reject the one string `dist` hands to `codesign` when nobody asked for an
    /// identity. The test exists so that tightening reds.
    #[test]
    fn one_dash_is_a_value_and_two_are_a_flag() {
        assert_eq!(after("--sign", &["-"]).as_deref(), Ok("-"));
        assert_eq!(after("--sign", &["-x"]).as_deref(), Ok("-x"));
        assert!(after("--sign", &["--x"]).is_err());
    }
}
