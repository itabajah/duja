//! Pure generation and parsing of the Duja `LaunchAgent` property list.
//!
//! The macOS autostart backend ([`mac`](super::mac)) is a `launchd` user agent:
//! a `.plist` in `~/Library/LaunchAgents/`. Only the *filesystem placement* of
//! that file is macOS-specific; composing its XML and reading a program path
//! back out of it are pure string operations, kept here so they are unit-tested
//! on **every** host (Windows/Linux included), independent of any real
//! `~/Library`.
//!
//! This module is compiled on macOS (where [`mac`](super::mac) uses it) and,
//! under `cfg(test)`, on every host (so the tests below run in ordinary
//! `cargo test`).

use std::path::Path;

/// The launchd job label, also the plist's base file name (`<LABEL>.plist`).
/// Matches the app's reverse-DNS bundle identifier used elsewhere in Duja.
pub(crate) const LABEL: &str = "io.github.itabajah.duja";

/// The plist file name Duja registers under (`io.github.itabajah.duja.plist`).
pub(crate) fn plist_file_name() -> String {
    format!("{LABEL}.plist")
}

/// Compose the `LaunchAgent` plist for an executable path.
///
/// Sets `Label`, a single-element `ProgramArguments` (the executable),
/// `RunAtLoad` = true (start at login), and `LimitLoadToSessionType` = `Aqua`
/// (only in a graphical login session, never in an ssh/background context). The
/// path is XML-escaped so a `&`/`<`/`>` in it cannot corrupt the document.
pub(crate) fn generate_plist(exe: &Path) -> String {
    let program = xml_escape(&exe.to_string_lossy());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>{LABEL}</string>\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         \t\t<string>{program}</string>\n\
         \t</array>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         \t<key>LimitLoadToSessionType</key>\n\
         \t<string>Aqua</string>\n\
         </dict>\n\
         </plist>\n"
    )
}

/// Extract `ProgramArguments[0]` from a plist, or `None` if the document has no
/// such entry.
///
/// A deliberately small scanner tuned to the launchd `<dict>`/`<array>` layout:
/// it finds the `ProgramArguments` key, then the first `<string>` of the array
/// that follows, and XML-unescapes it. Enough to recognize a plist Duja (or a
/// prior Duja version) wrote; not a general plist parser.
pub(crate) fn parse_program_argument0(plist: &str) -> Option<String> {
    let after_key = {
        let key = plist.find("<key>ProgramArguments</key>")?;
        &plist[key..]
    };
    let after_array = {
        let array = after_key.find("<array>")?;
        &after_key[array..]
    };
    let open = after_array
        .find("<string>")?
        .saturating_add("<string>".len());
    let inner = &after_array[open..];
    let close = inner.find("</string>")?;
    Some(xml_unescape(&inner[..close]))
}

/// Escape the five predefined XML entities relevant to element text.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Reverse [`xml_escape`]. `&amp;` is undone last so an escaped `&lt;`
/// (`&amp;lt;`) round-trips to `&lt;` rather than `<`.
fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn file_name_is_label_dot_plist() {
        assert_eq!(plist_file_name(), "io.github.itabajah.duja.plist");
        assert_eq!(LABEL, "io.github.itabajah.duja");
    }

    #[test]
    fn generated_plist_has_the_required_keys() {
        let plist = generate_plist(&PathBuf::from("/Applications/Duja.app/Contents/MacOS/duja"));
        assert!(plist.contains("<key>Label</key>"));
        assert!(plist.contains("<string>io.github.itabajah.duja</string>"));
        assert!(plist.contains("<key>ProgramArguments</key>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<true/>"));
        assert!(plist.contains("<key>LimitLoadToSessionType</key>"));
        assert!(plist.contains("<string>Aqua</string>"));
    }

    #[test]
    fn generate_then_parse_round_trips_the_exe_path() {
        let exe = PathBuf::from("/Users/me/Applications/Duja.app/Contents/MacOS/duja");
        let plist = generate_plist(&exe);
        assert_eq!(
            parse_program_argument0(&plist).as_deref(),
            Some("/Users/me/Applications/Duja.app/Contents/MacOS/duja")
        );
    }

    #[test]
    fn special_characters_in_the_path_round_trip() {
        // A path with XML-significant characters must survive escaping.
        let exe = PathBuf::from("/Users/a&b/<duja> app/duja");
        let plist = generate_plist(&exe);
        // The raw markup is escaped in the document...
        assert!(plist.contains("/Users/a&amp;b/&lt;duja&gt; app/duja"));
        assert!(!plist.contains("<duja>"));
        // ...and unescapes back to the original on parse.
        assert_eq!(
            parse_program_argument0(&plist).as_deref(),
            Some("/Users/a&b/<duja> app/duja")
        );
    }

    #[test]
    fn parse_returns_none_without_program_arguments() {
        let plist = "<?xml version=\"1.0\"?>\n<plist><dict>\
             <key>Label</key><string>io.github.itabajah.duja</string></dict></plist>";
        assert_eq!(parse_program_argument0(plist), None);
    }

    #[test]
    fn parse_returns_none_on_empty_input() {
        assert_eq!(parse_program_argument0(""), None);
    }

    #[test]
    fn parse_reads_a_hand_written_plist_layout() {
        // Tolerates a differently-formatted (but structurally standard) plist.
        let plist = "<plist version=\"1.0\"><dict>\n\
            <key>ProgramArguments</key>\n<array>\n\
            <string>/opt/duja/bin/duja</string>\n<string>--tray</string>\n</array>\n\
            <key>RunAtLoad</key><true/></dict></plist>";
        assert_eq!(
            parse_program_argument0(plist).as_deref(),
            Some("/opt/duja/bin/duja")
        );
    }
}
