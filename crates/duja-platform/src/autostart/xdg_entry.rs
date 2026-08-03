//! Pure rendering of the XDG autostart `.desktop` entry.
//!
//! The Linux autostart backend ([`linux`](super::linux)) writes one file into
//! `~/.config/autostart`. Only the *filesystem placement* of that file is
//! Linux-specific; composing its contents is a pure string operation, kept here
//! so it is unit-tested on **every** host, independent of any real `$HOME`.
//!
//! Same arrangement and same reason as the macOS `plist` module (backticks
//! rather than a link, because that module is not compiled on this target):
//! compiled on Linux, where the backend uses it, and under `cfg(test)` on every
//! host so these tests run in an ordinary `cargo test`.

use std::path::Path;

/// Render the autostart entry for `exe`.
///
/// Keys chosen from the Desktop Entry Specification plus the one GNOME
/// extension that is universally honoured:
///
/// - `Type=Application` and `Exec` are the only two the spec requires for a
///   launchable entry; `Name` is required for any entry at all.
/// - `Terminal=false` stops a session manager opening a terminal window around
///   a tray application.
/// - `X-GNOME-Autostart-enabled=true` is redundant on a fresh install (absence
///   means enabled) and is written anyway, because GNOME Tweaks and several
///   session managers set it to `false` when a user disables an entry through
///   *their* UI. Without it, re-enabling from Duja would write a file GNOME
///   still considers disabled, and the toggle would appear to do nothing.
pub(super) fn desktop_entry(exe: &Path) -> String {
    // `Exec` is a command line, not a path, and the spec reserves characters in
    // it. An unquoted path containing a space parses as several arguments — an
    // install under `~/My Apps/` is enough to hit it.
    let quoted = quote_exec(&exe.to_string_lossy());
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Duja\n\
         Comment=Monitor brightness control\n\
         Exec={quoted}\n\
         Terminal=false\n\
         Categories=Utility;\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

/// Quote a program path for a `.desktop` `Exec` value.
///
/// **Two levels of escaping, not one.** The Desktop Entry Specification defines
/// `Exec` as a command line *inside* a key-file `string` value, and each level
/// has its own rules:
///
/// 1. **The `Exec` argument.** An argument containing reserved characters is
///    enclosed in double quotes, and within them a backslash, a double quote, a
///    backtick or a dollar sign takes a preceding backslash. Separately, `%`
///    introduces a **field code** (`%f`, `%u`, `%c`…), so a literal percent must
///    be doubled or the launcher consumes the character after it.
/// 2. **The key-file value.** The result is then escaped as a `string`: a
///    backslash becomes two, and a newline, tab or carriage return becomes
///    `\n`, `\t`, `\r`.
///
/// Applying only the first is the subtle version of this bug, and it is what the
/// specification's own note about *four* successive backslashes warns against: a
/// path ending in a backslash would emit two, which unescape to one and leave
/// the command-line parser with an unterminated quote — killing the whole
/// `Exec`, not just that argument.
///
/// Applied unconditionally rather than only when a reserved character is
/// present. A path that needs no quoting is unharmed by having it, and a rule
/// that fires "only when needed" has a second branch that no ordinary install
/// would ever exercise — which is the branch that would be wrong.
fn quote_exec(path: &str) -> String {
    escape_key_file(&quote_argument(path))
}

/// Level 1: wrap `path` as a single `Exec` argument.
fn quote_argument(path: &str) -> String {
    let mut out = String::with_capacity(path.len().saturating_add(2));
    out.push('"');
    for ch in path.chars() {
        match ch {
            // A field-code marker. `/opt/100%fun/duja` would otherwise have `%f`
            // substituted by the launcher and start a path that does not exist —
            // silently, at login, where nobody is watching.
            '%' => out.push_str("%%"),
            '\\' | '"' | '`' | '$' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Level 2: escape a value for the key-file `string` format.
///
/// The format is line-based, so a raw newline or tab would truncate the `Exec`
/// and turn the remainder into a stray line a strict parser rejects.
fn escape_key_file(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '\n' => out.push_str(r"\n"),
            '\t' => out.push_str(r"\t"),
            '\r' => out.push_str(r"\r"),
            _ => out.push(ch),
        }
    }
    out
}

/// Whether a `.desktop` entry's contents mean "launch me".
///
/// Two keys can say no, and both are written by something other than Duja:
/// `Hidden=true` is the specification's own "ignore this entry", and
/// `X-GNOME-Autostart-enabled=false` is what GNOME Tweaks writes when a user
/// disables an entry there. Absence of either means enabled, which is why this
/// reads for a *disabling* value rather than requiring an enabling one — a
/// hand-written entry with neither key is enabled, and must stay that way.
///
/// Values are compared case-insensitively and trimmed: the key-file format
/// permits surrounding whitespace, and `True`/`TRUE` appear in the wild.
pub(super) fn is_entry_enabled(contents: &str) -> bool {
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().to_ascii_lowercase();
        match key.trim() {
            "Hidden" if value == "true" => return false,
            "X-GNOME-Autostart-enabled" if value == "false" => return false,
            _ => {}
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// The `Exec` line of a rendered entry.
    fn exec_line(entry: &str) -> &str {
        entry
            .lines()
            .find_map(|line| line.strip_prefix("Exec="))
            .expect("every entry has an Exec line")
    }

    #[test]
    fn an_ordinary_path_renders_a_launchable_entry() {
        let entry = desktop_entry(&PathBuf::from("/usr/local/bin/duja"));

        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("\nType=Application\n"));
        assert!(entry.contains("\nName=Duja\n"));
        assert!(entry.contains("\nTerminal=false\n"));
        assert_eq!(exec_line(&entry), "\"/usr/local/bin/duja\"");
        // The spec wants a trailing newline; a session manager that reads the
        // file line-wise would otherwise drop the last key.
        assert!(entry.ends_with('\n'));
    }

    /// The reason the quoting exists. An install under a path with a space is
    /// ordinary on Linux (`~/My Apps/`, `~/Downloads/duja 0.3.0/`), and an
    /// unquoted `Exec` would launch the first word with the rest as arguments.
    #[test]
    fn a_path_with_a_space_stays_one_argument() {
        let entry = desktop_entry(&PathBuf::from("/home/ana/My Apps/duja"));

        assert_eq!(exec_line(&entry), "\"/home/ana/My Apps/duja\"");
    }

    /// The four characters escaped inside the quotes, through **both** levels.
    /// A path containing `$` is the reachable one: a parser that expanded it
    /// would launch a path that does not exist.
    ///
    /// The expectation is written out rather than computed, because computing it
    /// with the functions under test would assert nothing.
    #[test]
    fn the_reserved_characters_are_escaped_through_both_levels() {
        let entry = desktop_entry(&PathBuf::from(r#"/opt/a$b`c\d"e/duja"#));

        // Level 1 produces  "/opt/a\$b\`c\\d\"e/duja"
        // and level 2 doubles every backslash in it.
        assert_eq!(exec_line(&entry), r#""/opt/a\\$b\\`c\\\\d\\"e/duja""#);
    }

    /// `%` introduces a field code, so a literal one must be doubled. An install
    /// under a percent-bearing directory would otherwise have `%f`/`%u`/`%c`
    /// substituted and start a path that does not exist.
    #[test]
    fn a_percent_in_the_path_is_doubled_so_it_is_not_a_field_code() {
        let entry = desktop_entry(&PathBuf::from("/opt/100%fun/duja"));

        assert_eq!(exec_line(&entry), r#""/opt/100%%fun/duja""#);
    }

    /// The specification's own worked example: a path ending in a backslash needs
    /// **four** successive backslashes in the file. One level of escaping emits
    /// two, which unescape to one and leave the command-line parser with an
    /// unterminated quote — the whole `Exec` fails, not just that argument.
    #[test]
    fn a_trailing_backslash_survives_both_levels() {
        let entry = desktop_entry(&PathBuf::from(r"/opt/odd\"));

        assert_eq!(exec_line(&entry), r#""/opt/odd\\\\""#);
    }

    /// A raw newline or tab would truncate the `Exec` and turn the remainder
    /// into a stray line a strict parser rejects.
    #[test]
    fn control_characters_become_key_file_escapes() {
        assert_eq!(escape_key_file("a\nb"), r"a\nb");
        assert_eq!(escape_key_file("a\tb"), r"a\tb");
        assert_eq!(escape_key_file("a\rb"), r"a\rb");
    }

    /// Quoting is unconditional, so the simple case and the hard case go through
    /// exactly one code path. Asserted directly, because "it also works for
    /// ordinary paths" is what a conditional implementation would break.
    #[test]
    fn quoting_is_unconditional() {
        assert_eq!(quote_exec("/usr/bin/duja"), "\"/usr/bin/duja\"");
        assert_eq!(quote_exec(""), "\"\"");
    }

    /// An entry a desktop has been told to ignore must read as **disabled**, or
    /// Duja's toggle shows ON for something that never launches and the user has
    /// no disabled state to re-enable from.
    #[test]
    fn an_entry_a_desktop_disabled_reads_as_disabled() {
        let base = desktop_entry(&PathBuf::from("/usr/bin/duja"));
        assert!(
            is_entry_enabled(&base),
            "a freshly written entry is enabled"
        );

        // What GNOME Tweaks does to the entry Duja wrote.
        let tweaked = base.replace(
            "X-GNOME-Autostart-enabled=true",
            "X-GNOME-Autostart-enabled=false",
        );
        assert!(!is_entry_enabled(&tweaked));

        // The specification's own key, which Duja never writes and other tools do.
        assert!(!is_entry_enabled(&format!(
            "{base}Hidden=true
"
        )));
    }

    /// Absence means enabled. An entry written by hand, or by an older Duja, has
    /// neither key and must not read as disabled.
    #[test]
    fn an_entry_with_neither_key_is_enabled() {
        assert!(is_entry_enabled(
            "[Desktop Entry]
Type=Application
Name=Duja
Exec=/usr/bin/duja
"
        ));
        assert!(is_entry_enabled(""));
    }

    /// The key-file format permits whitespace around values, and `True`/`TRUE`
    /// appear in the wild. A case-sensitive comparison would read a disabled
    /// entry as enabled.
    #[test]
    fn the_disabling_values_are_matched_loosely() {
        for line in [
            "Hidden=true",
            "Hidden = TRUE ",
            "Hidden=True",
            "X-GNOME-Autostart-enabled=false",
            "X-GNOME-Autostart-enabled = FALSE",
        ] {
            assert!(
                !is_entry_enabled(&format!(
                    "[Desktop Entry]
{line}
"
                )),
                "{line}"
            );
        }
        // The opposite values must not disable it.
        assert!(is_entry_enabled(
            "Hidden=false
X-GNOME-Autostart-enabled=true
"
        ));
    }

    /// GNOME Tweaks writes `X-GNOME-Autostart-enabled=false` when a user
    /// disables an entry there. Duja rewrites the file on enable, so the key
    /// must be present and true, or re-enabling from Duja would silently leave
    /// GNOME's own "disabled" in place.
    #[test]
    fn the_gnome_enabled_key_is_written_rather_than_left_to_default() {
        let entry = desktop_entry(&PathBuf::from("/usr/bin/duja"));

        assert!(entry.contains("\nX-GNOME-Autostart-enabled=true\n"));
    }

    /// Every line is a `Key=Value` or the group header. A stray line would make
    /// a strict parser reject the whole file, and the session manager that does
    /// so is the one that silently stops launching Duja.
    #[test]
    fn every_line_is_a_group_header_or_a_key_value_pair() {
        let entry = desktop_entry(&PathBuf::from("/usr/bin/duja"));

        let mut lines = entry.lines();
        assert_eq!(lines.next(), Some("[Desktop Entry]"));
        for line in lines {
            assert!(
                line.contains('=') && !line.starts_with('='),
                "not a key-value line: {line:?}"
            );
        }
    }
}
