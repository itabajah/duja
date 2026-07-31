//! The `--version` argument, validated once at the edge.
//!
//! `dist` puts the version it is handed into places that are **not** plain
//! strings: an XML property list (macOS `Info.plist`), a disk-image volume name,
//! artifact file names, and — on Windows — a single-quoted PowerShell literal in
//! the `Compress-Archive` command line. Each of those has its own metacharacter
//! (`<`/`&` for XML, `'` for PowerShell, path separators for file names).
//!
//! Rather than teach every consumer a different escape, the value is parsed into
//! a [`Version`] at the point it enters the program, and only a `Version` can be
//! formatted into any of them. The accepted alphabet is deliberately narrower
//! than "a valid version": it is the set of characters that are inert in *all*
//! of those contexts, which keeps the escaping question from ever arising.
//!
//! This is stricter than the code it replaces — the previous `dist` interpolated
//! the raw argument straight into the PowerShell command — and the tightening is
//! the point.

use std::fmt;

/// The longest version string accepted, in bytes.
///
/// An arbitrary sanity cap, and named as one rather than dressed up: none of the
/// contexts a version reaches has a limit anywhere near it (an HFS+ volume name
/// allows 255 characters, and every path here is well inside `PATH_MAX`). It is
/// here so a pasted-in file or a shell mishap becomes an error message instead
/// of a 4 KB artifact name, not because 64 is a boundary of anything.
const MAX_LEN: usize = 64;

/// A release version that is safe to place in a file name, an XML text node, a
/// disk-image volume name, and a quoted shell literal.
///
/// Construct with [`Version::parse`]; there is no other way to make one, so a
/// function taking a `Version` cannot be handed an unvalidated string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Version(String);

impl Version {
    /// Validate `raw` and wrap it.
    ///
    /// Accepts ASCII alphanumerics plus `.`, `-`, `+` and `_` — enough for
    /// `1.2.3`, `1.2.3-beta.1` and `1.2.3+build.7`, and nothing that is special
    /// to XML, a shell, or a path.
    ///
    /// # Errors
    /// Returns a human-readable message if `raw` is empty, longer than
    /// [`MAX_LEN`], or contains a character outside that set. The message names
    /// the offending character so a typo is obvious.
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        if raw.is_empty() {
            return Err("`--version` is empty".to_owned());
        }
        if raw.len() > MAX_LEN {
            return Err(format!(
                "`--version` is {} bytes; the limit is {MAX_LEN}",
                raw.len()
            ));
        }
        if let Some(bad) = raw
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_')))
        {
            return Err(format!(
                "`--version` contains {bad:?}; only ASCII letters, digits, `.`, `-`, `+` and `_` are allowed"
            ));
        }
        Ok(Version(raw.to_owned()))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_release_version_parses() {
        for raw in [
            "0.1.5",
            "1.0.0",
            "0.2.0-beta.1",
            "1.2.3+build.7",
            "0.2.0_rc1",
        ] {
            let parsed = Version::parse(raw).expect("ordinary version");
            assert_eq!(parsed.to_string(), raw);
        }
    }

    #[test]
    fn an_empty_version_is_rejected() {
        let err = Version::parse("").expect_err("empty");
        assert!(err.contains("empty"), "{err}");
    }

    /// The characters this type exists to keep out, one per hostile context:
    /// `'` would close the PowerShell literal `dist` builds for
    /// `Compress-Archive`; `<`, `&` and `"` would corrupt the `Info.plist` XML;
    /// the separators would escape the staging directory in a file name; a
    /// space would split an argument in a copy-pasted command line.
    #[test]
    fn a_version_that_could_escape_its_context_is_rejected() {
        for raw in [
            "1.0'; rm -rf /; #",
            "1.0<b>",
            "1.0&amp;",
            "1.0\"",
            "../../etc/passwd",
            "1.0\\2",
            "1.0 2",
            "1.0\n2",
            "1.0$(id)",
        ] {
            let err = Version::parse(raw).expect_err(raw);
            assert!(err.contains("only ASCII letters"), "{raw}: {err}");
        }
    }

    #[test]
    fn an_absurdly_long_version_is_rejected() {
        let raw = "1".repeat(MAX_LEN.saturating_add(1));
        let err = Version::parse(&raw).expect_err("too long");
        assert!(err.contains("the limit is"), "{err}");
        // The boundary itself is accepted — the bound is a cap, not an off-by-one.
        assert!(Version::parse(&"1".repeat(MAX_LEN)).is_ok());
    }
}
