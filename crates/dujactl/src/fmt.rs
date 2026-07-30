//! Pure formatting and brightness-scaling helpers for `dujactl`.
//!
//! Kept free of I/O and hardware so they are unit testable in isolation.

use duja_core::input_source;
use duja_core::model::{Capabilities, DisplayKind, Feature};
use duja_core::quirks::ResolvedQuirks;

/// Indent for `doctor`'s top-level summary lines, and the width its labels are
/// padded to so the values line up in a pasted report.
const SUMMARY_LABEL: usize = 19;
/// Indent for `doctor`'s per-display detail lines.
const DETAIL_INDENT: &str = "      ";
/// Width the per-display detail labels are padded to.
const DETAIL_LABEL: usize = 13;

/// One `doctor` summary line: two-space indent, padded label, value.
pub fn summary_line(label: &str, value: &str) -> String {
    format!("  {label:<SUMMARY_LABEL$}{value}")
}

/// One per-display detail line: six-space indent, padded label, value.
fn detail_line(label: &str, value: &str) -> String {
    format!("{DETAIL_INDENT}{label:<DETAIL_LABEL$}{value}")
}

/// The `dujactl:` identity line every `doctor` run prints first.
///
/// This is the line that makes a pasted diagnostic attributable: without it a
/// quirk report says nothing about *which build* produced it or *which platform*
/// it ran on, and both change the meaning of everything below it. `version` is
/// the `dujactl` binary's own `CARGO_PKG_VERSION` (not a library's `version()`,
/// so the label is self-evidently true of the thing that printed it), and `os` /
/// `arch` are [`std::env::consts`], which keeps this dependency-free — a finer OS
/// build number would mean new FFI for a header line.
pub fn report_header(version: &str, os: &str, arch: &str) -> String {
    summary_line("dujactl:", &format!("{version} ({os} {arch})"))
}

/// The `ipc server:` line, given whether the local IPC server answered.
///
/// The reachability itself is real quirk context — a quirk may only show up with
/// the app's engine pacing writes alongside `dujactl` — but it must not imply the
/// diagnostics *came through* the app: `doctor` never uses IPC. It opens the
/// hardware directly in both branches, which is exactly why it can print a raw
/// capability string at all.
pub fn ipc_line(reachable: bool) -> String {
    let value = if reachable {
        "reachable (the app is running; doctor still reads the hardware directly)"
    } else {
        "not running (doctor reads the hardware directly)"
    };
    summary_line("ipc server:", value)
}

/// A short, stable label for a [`DisplayKind`] (its physical provenance).
pub fn kind_label(kind: DisplayKind) -> &'static str {
    match kind {
        DisplayKind::ExternalDdc => "external",
        DisplayKind::InternalPanel => "internal",
    }
}

/// A short label for a [`Feature`].
fn feature_label(feature: Feature) -> &'static str {
    match feature {
        Feature::Brightness => "brightness",
        Feature::Contrast => "contrast",
        Feature::InputSource => "input",
    }
}

/// Comma-separated feature names for a capability set, or `"-"` if empty.
pub fn features_label(caps: &Capabilities) -> String {
    if caps.features.is_empty() {
        return "-".to_owned();
    }
    caps.features
        .iter()
        .map(|f| feature_label(*f))
        .collect::<Vec<_>>()
        .join(",")
}

/// Scale a user percent (0–100) onto a raw feature range: `raw = pct*max/100`.
///
/// `pct` is clamped to 100; integer math with no overflow and no panic.
pub fn pct_to_raw(pct: u8, max: u16) -> u16 {
    let scaled = u32::from(pct.min(100))
        .saturating_mul(u32::from(max))
        .checked_div(100)
        .unwrap_or(0);
    u16::try_from(scaled).unwrap_or(max)
}

/// Reflect a raw hardware value back to a percent (inverse of [`pct_to_raw`]).
///
/// Guards a zero `max` (returns 0) so it never divides by zero.
pub fn raw_to_pct(current: u16, max: u16) -> u8 {
    let pct = u32::from(current)
        .saturating_mul(100)
        .checked_div(u32::from(max))
        .unwrap_or(0);
    u8::try_from(pct.min(100)).unwrap_or(100)
}

/// A one-line summary of a display's resolved quirks, or `"(none)"`.
pub fn quirk_summary(quirks: &ResolvedQuirks) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(ms) = quirks.min_write_gap_ms {
        parts.push(format!("min_gap={ms}ms"));
    }
    if let Some(retry) = quirks.caps_retry {
        parts.push(format!("caps_retry={retry}"));
    }
    if let Some(max) = quirks.max_brightness {
        parts.push(format!("max_brightness={max}"));
    }
    if quirks.verify_writes {
        parts.push("verify_writes".to_owned());
    }
    if quirks.no_input_switch {
        parts.push("no_input_switch".to_owned());
    }
    if quirks.caps_unreliable {
        parts.push("caps_unreliable".to_owned());
    }
    if quirks.ddc_broken {
        parts.push("ddc_broken".to_owned());
    }
    if parts.is_empty() {
        "(none)".to_owned()
    } else {
        parts.join(", ")
    }
}

/// The quirk block `doctor` prints under a display: the one-line flag summary,
/// then one line per accumulated note.
///
/// The notes are what [ADR-0007] promises the `doctor` report can cite to explain
/// *why* a monitor is being driven conservatively. [`ResolvedQuirks::notes`] has
/// accumulated them (least-specific first) since P2, but nothing rendered them
/// until now, so the ADR asserted an output that did not exist.
///
/// This is not a latent line waiting for a future quirk: the one entry the
/// embedded DB ships (`MSI-30B6`) carries a note, so it prints on the dev box's
/// own monitor today and is the only place the pacing/caps-retry/verify verdicts
/// on the line above it are *explained* rather than just listed.
///
/// [ADR-0007]: https://github.com/itabajah/duja/blob/main/docs/adr/0007-config-schema-and-migrations.md
pub fn quirk_lines(quirks: &ResolvedQuirks) -> Vec<String> {
    let mut lines = vec![detail_line("quirks:", &quirk_summary(quirks))];
    lines.extend(
        quirks
            .notes
            .iter()
            .map(|note| detail_line("note:", note.as_str())),
    );
    lines
}

/// The per-display probe block `doctor --report` prints: what the monitor
/// actually *reported*, as opposed to what Duja already believes about its EDID.
///
/// This is the substance of a monitor quirk report, and each line answers one of
/// the symptoms the issue template's dropdown offers:
///
/// - `features` / `caps` — "capability string wrong or missing". The raw MCCS
///   string is *the* artifact: it is what every other verdict here is derived
///   from, and `(none reported)` is itself the finding for a monitor that will not
///   answer a capabilities request.
/// - `brightness` / `hw range` — "wrong/lying brightness range". A monitor
///   claiming `max` 0, or a `hardware_range: false` verdict on a display the user
///   believes is DDC-capable, is invisible without these.
/// - `inputs` — "input-source switching broken". The set Duja will *let* the user
///   select: the capability string's `0x60` value list intersected with quirks.
///
/// `brightness` is the `(current, max)` read, or `None` when the read failed —
/// which is a finding of its own (a display that probes but will not answer VCP
/// `0x10`), not an error.
pub fn probe_report(caps: &Capabilities, brightness: Option<(u16, u16)>) -> Vec<String> {
    let range = brightness.map_or_else(
        || "unreadable (the display did not answer VCP 0x10)".to_owned(),
        |(current, max)| format!("{current}/{max} ({}%)", raw_to_pct(current, max)),
    );
    let inputs = if caps.allowed_inputs.is_empty() {
        "(none advertised)".to_owned()
    } else {
        caps.allowed_inputs
            .iter()
            .map(|&code| format!("{} ({code:#04x})", input_source::label(code)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    vec![
        detail_line("features:", &features_label(caps)),
        detail_line("brightness:", &range),
        detail_line(
            "hw-range:",
            if caps.hardware_range {
                "yes"
            } else {
                "no (software-only dimming)"
            },
        ),
        detail_line("inputs:", &inputs),
        detail_line(
            "caps:",
            caps.raw_capabilities
                .as_deref()
                .unwrap_or("(none reported)"),
        ),
    ]
}

/// The per-display block `doctor --report` prints when a display could not be
/// opened or probed at all.
///
/// A probe failure is the most valuable thing a quirk report can carry, not an
/// error: "enumerates but DDC is dead" is otherwise byte-identical to a healthy
/// monitor in this output. So it is printed as a finding and `doctor` still exits
/// 0 — the reporter has something to paste rather than a non-zero exit and no
/// diagnostic.
pub fn probe_failure(reason: &str) -> Vec<String> {
    vec![detail_line(
        "probe:",
        &format!("FAILED: {reason} — enumerated, but not answering DDC"),
    )]
}

/// What one display answered when `doctor --report` asked it: its probed
/// [`Capabilities`] plus the `(current, max)` brightness read (`None` when only
/// that read failed), or the reason it could not be asked at all.
pub type ProbeOutcome = Result<(Capabilities, Option<(u16, u16)>), String>;

/// One display's whole block in `doctor`'s output: the identity line, the quirk
/// block, and — only under `--report` — the probe block.
///
/// `probe` is `None` for plain `doctor`. That is the split between the command's
/// two audiences: plain `doctor` reports what Duja *believes* about this EDID (the
/// `quirks:` line is `QuirkDb::resolve`) and costs no DDC traffic; `--report` adds
/// what the monitor itself *answered*, which is what the monitor-quirk issue
/// template promises and what the belief cannot substitute for.
pub fn display_block(
    kind: DisplayKind,
    id: &str,
    name: &str,
    quirks: &ResolvedQuirks,
    probe: Option<&ProbeOutcome>,
) -> Vec<String> {
    let mut lines = vec![format!("  [{}] {id} ({name})", kind_label(kind))];
    lines.extend(quirk_lines(quirks));
    match probe {
        None => {}
        Some(Ok((caps, brightness))) => lines.extend(probe_report(caps, *brightness)),
        Some(Err(reason)) => lines.extend(probe_failure(reason)),
    }
    lines
}

/// Render an aligned text table: a header row, a dashed rule, then the rows.
pub fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row.iter()) {
            *w = (*w).max(cell.len());
        }
    }

    let mut lines: Vec<String> = Vec::new();
    lines.push(format_row(&widths, headers.iter().copied()));
    lines.push(rule(&widths));
    for row in rows {
        lines.push(format_row(&widths, row.iter().map(String::as_str)));
    }
    lines.join("\n")
}

/// Pad and join one row's cells with a two-space gutter.
fn format_row<'a>(widths: &[usize], cells: impl Iterator<Item = &'a str>) -> String {
    widths
        .iter()
        .zip(cells)
        .map(|(w, c)| format!("{c:<w$}"))
        .collect::<Vec<_>>()
        .join("  ")
        .trim_end()
        .to_owned()
}

/// A dashed rule sized to the column widths.
fn rule(widths: &[usize]) -> String {
    widths
        .iter()
        .map(|w| "-".repeat(*w))
        .collect::<Vec<_>>()
        .join("  ")
}

#[cfg(test)]
mod tests {
    use super::{
        display_block, features_label, ipc_line, kind_label, pct_to_raw, probe_failure,
        probe_report, quirk_lines, quirk_summary, raw_to_pct, render_table, report_header,
    };
    use duja_core::model::{Capabilities, DisplayKind, Feature};
    use duja_core::quirks::ResolvedQuirks;

    /// The `n`th rendered line, or `""` — indexing is denied by the lint wall even
    /// in tests, and a missing line should fail the assertion that wanted it rather
    /// than panic out of the test before printing its message.
    fn nth(lines: &[String], n: usize) -> &str {
        lines.get(n).map_or("", String::as_str)
    }

    /// The header is the whole product of `doctor`'s identity line, and it is what
    /// makes a pasted report attributable at all. Pinned as a full line, so an
    /// emptied or deleted `report_header` reds here rather than passing silently:
    /// before this test existed, replacing the entire body of the feature with
    /// `let _ = report;` left all 913 tests green.
    ///
    /// It must carry the version, the OS and the arch, and it must be labelled
    /// `dujactl:` — a report that says only "0.1.5" cannot be attributed to a
    /// binary, and one that says only `windows` cannot be attributed to a build.
    #[test]
    fn report_header_names_the_build_and_the_platform() {
        let line = report_header("0.1.5", "windows", "x86_64");
        assert_eq!(line, "  dujactl:           0.1.5 (windows x86_64)");
        // Restated as properties, so a reformat that keeps the contract still
        // passes but a drop of any of the three parts cannot.
        assert!(line.contains("dujactl:"), "the label names the binary");
        assert!(line.contains("0.1.5"), "the build that produced the report");
        assert!(line.contains("windows"), "the OS it ran on");
        assert!(line.contains("x86_64"), "the architecture it ran on");
    }

    /// `doctor` NEVER goes over IPC — it always opens the hardware directly — so
    /// the reachability line must not imply the diagnostics below it were served
    /// by the running app. The fact is kept (whether the app was up is real quirk
    /// context: a quirk may only appear with the engine pacing writes), the
    /// implication is not.
    #[test]
    fn ipc_line_reports_reachability_without_claiming_it_served_the_report() {
        let up = ipc_line(true);
        assert!(up.contains("reachable"), "{up}");
        assert!(
            up.contains("reads the hardware directly"),
            "must say where the diagnostics actually came from: {up}"
        );
        assert!(
            !up.contains("will serve dujactl"),
            "must not imply the app served this report: {up}"
        );
        let down = ipc_line(false);
        assert!(down.contains("not running"), "{down}");
        assert!(down.contains("reads the hardware directly"), "{down}");
    }

    #[test]
    fn kind_labels_are_stable() {
        assert_eq!(kind_label(DisplayKind::ExternalDdc), "external");
        assert_eq!(kind_label(DisplayKind::InternalPanel), "internal");
    }

    #[test]
    fn features_label_lists_or_dashes() {
        assert_eq!(features_label(&Capabilities::default()), "-");
        let caps = Capabilities {
            features: [Feature::Brightness, Feature::InputSource]
                .into_iter()
                .collect(),
            hardware_range: true,
            raw_capabilities: None,
            allowed_inputs: Vec::new(),
        };
        assert_eq!(features_label(&caps), "brightness,input");
    }

    #[test]
    fn pct_to_raw_maps_onto_range() {
        assert_eq!(pct_to_raw(0, 100), 0);
        assert_eq!(pct_to_raw(50, 100), 50);
        assert_eq!(pct_to_raw(100, 100), 100);
        // A non-100 max scales proportionally.
        assert_eq!(pct_to_raw(50, 200), 100);
        assert_eq!(pct_to_raw(25, 80), 20);
        // Over-range percent clamps.
        assert_eq!(pct_to_raw(200, 50), 50);
    }

    #[test]
    fn raw_to_pct_inverts_and_guards_zero_max() {
        assert_eq!(raw_to_pct(0, 100), 0);
        assert_eq!(raw_to_pct(50, 100), 50);
        assert_eq!(raw_to_pct(200, 200), 100);
        assert_eq!(raw_to_pct(5, 0), 0);
    }

    #[test]
    fn quirk_summary_reports_none_or_flags() {
        assert_eq!(quirk_summary(&ResolvedQuirks::default()), "(none)");
        let quirks = ResolvedQuirks {
            min_write_gap_ms: Some(120),
            verify_writes: true,
            ddc_broken: true,
            ..ResolvedQuirks::default()
        };
        let summary = quirk_summary(&quirks);
        assert!(summary.contains("min_gap=120ms"));
        assert!(summary.contains("verify_writes"));
        assert!(summary.contains("ddc_broken"));
    }

    /// ADR-0007 claims the `doctor` report "can cite accumulated quirk `notes`".
    /// `ResolvedQuirks.notes` accumulated them but `quirk_summary` never rendered
    /// one, so the ADR asserted an output that did not exist.
    #[test]
    fn quirk_lines_render_the_accumulated_notes_adr_0007_promises() {
        let none = quirk_lines(&ResolvedQuirks::default());
        assert_eq!(none.len(), 1, "no notes ⇒ just the flag summary: {none:?}");
        assert!(nth(&none, 0).contains("quirks:"));
        assert!(nth(&none, 0).contains("(none)"));

        let quirks = ResolvedQuirks {
            min_write_gap_ms: Some(120),
            notes: vec!["ignores writes for ~100 ms after DPMS wake".to_owned()],
            ..ResolvedQuirks::default()
        };
        let lines = quirk_lines(&quirks);
        assert_eq!(lines.len(), 2, "the flag summary plus one note: {lines:?}");
        assert!(nth(&lines, 0).contains("min_gap=120ms"));
        assert!(
            nth(&lines, 1).contains("ignores writes for ~100 ms after DPMS wake"),
            "the note must be rendered verbatim: {:?}",
            nth(&lines, 1)
        );
    }

    /// The monitor-quirk template promises the paste contains "the monitor
    /// identity and **probed capabilities**". Four of the six symptoms its
    /// required dropdown offers are only evidenced by these lines: the raw MCCS
    /// string, the probed feature set, the current/max range plus the
    /// hardware-range verdict, and the allowed input set.
    #[test]
    fn probe_report_carries_every_artifact_a_quirk_report_needs() {
        let caps = Capabilities {
            features: [Feature::Brightness, Feature::InputSource]
                .into_iter()
                .collect(),
            hardware_range: true,
            raw_capabilities: Some("(vcp(10 60(11 0F)))".to_owned()),
            allowed_inputs: vec![0x11, 0x0F],
        };
        let block = probe_report(&caps, Some((42, 100))).join("\n");

        // "Capability string wrong or missing" — the raw string, verbatim.
        assert!(block.contains("(vcp(10 60(11 0F)))"), "{block}");
        // The probed feature set, not what the quirk DB believes.
        assert!(block.contains("brightness,input"), "{block}");
        // "Wrong/lying brightness range" — the live range and the verdict.
        assert!(block.contains("42/100 (42%)"), "{block}");
        assert!(
            block.contains("hw-range:") && block.contains("yes"),
            "{block}"
        );
        // "Input-source switching broken" — the set Duja will let the user pick.
        assert!(block.contains("hdmi1 (0x11)"), "{block}");
        assert!(block.contains("dp1 (0x0f)"), "{block}");
    }

    /// The empty/absent answers are findings in their own right and must each be
    /// stated, not omitted: an absent capability string IS the report for
    /// "capability string missing", and a `max` the display would not give up is
    /// the report for "commands ignored".
    #[test]
    fn probe_report_states_the_absences_rather_than_omitting_them() {
        let block = probe_report(&Capabilities::default(), None).join("\n");
        assert!(block.contains("(none reported)"), "no caps string: {block}");
        assert!(block.contains("unreadable"), "no 0x10 answer: {block}");
        assert!(block.contains("VCP 0x10"), "and which read failed: {block}");
        assert!(
            block.contains("software-only"),
            "hardware_range: false is a verdict the reporter needs: {block}"
        );
        assert!(
            block.contains("(none advertised)"),
            "no switchable inputs: {block}"
        );
        assert_eq!(
            block.lines().count(),
            5,
            "every line is present even when every answer is empty: {block}"
        );
    }

    /// "Enumerates but DDC is dead" was byte-identical to a healthy monitor in
    /// this output. A probe failure is the report, so it is printed as a finding.
    #[test]
    fn probe_failure_is_reported_as_a_finding() {
        let lines = probe_failure("Timeout");
        assert_eq!(lines.len(), 1);
        let line = nth(&lines, 0);
        assert!(line.contains("probe:"), "{line:?}");
        assert!(line.contains("FAILED"), "{line:?}");
        assert!(line.contains("Timeout"), "{line:?}");
        assert!(
            line.contains("enumerated"),
            "say what the failure means: {line:?}"
        );
    }

    /// The block a display contributes: identity, then belief (quirks), then —
    /// only under `--report` — what the hardware answered. The `None` case is the
    /// whole of plain `doctor`'s per-display output, so it must not leak a probe
    /// line, and the `Err` case must still produce a block.
    #[test]
    fn display_block_adds_the_probe_only_when_one_was_taken() {
        let quirks = ResolvedQuirks::default();
        let plain = display_block(
            DisplayKind::ExternalDdc,
            "MSI-30B6-X",
            "MSI MP273QP",
            &quirks,
            None,
        )
        .join("\n");
        assert!(
            plain.contains("[external] MSI-30B6-X (MSI MP273QP)"),
            "{plain}"
        );
        assert!(plain.contains("quirks:"), "{plain}");
        assert!(!plain.contains("caps:"), "no probe was taken: {plain}");
        assert!(!plain.contains("hw-range:"), "no probe was taken: {plain}");
        // The id used to be printed twice (`edid id:` repeated the line above it).
        assert_eq!(plain.matches("MSI-30B6-X").count(), 1, "{plain}");

        let caps = Capabilities {
            hardware_range: true,
            raw_capabilities: Some("(vcp(10))".to_owned()),
            ..Capabilities::default()
        };
        let probed = display_block(
            DisplayKind::ExternalDdc,
            "MSI-30B6-X",
            "MSI MP273QP",
            &quirks,
            Some(&Ok((caps, Some((42, 100))))),
        )
        .join("\n");
        assert!(probed.contains("(vcp(10))"), "{probed}");
        assert!(probed.contains("42/100 (42%)"), "{probed}");

        let failed = display_block(
            DisplayKind::ExternalDdc,
            "MSI-30B6-X",
            "MSI MP273QP",
            &quirks,
            Some(&Err("Timeout".to_owned())),
        )
        .join("\n");
        assert!(
            failed.contains("probe:") && failed.contains("Timeout"),
            "{failed}"
        );
    }

    /// Every per-display line shares one indent and one label column, so a paste
    /// into an issue reads as a block rather than a ragged list — and every label
    /// is a single token, so a maintainer can grep a pile of pasted reports for
    /// `caps:` or `hw-range:` without the labels themselves needing quoting.
    #[test]
    fn every_per_display_line_shares_the_detail_column() {
        let quirks = ResolvedQuirks {
            notes: vec!["a note".to_owned()],
            ..ResolvedQuirks::default()
        };
        let mut lines = quirk_lines(&quirks);
        lines.extend(probe_report(&Capabilities::default(), Some((1, 1))));
        lines.extend(probe_failure("Timeout"));
        for line in &lines {
            assert!(line.starts_with("      "), "six-space indent: {line:?}");
            assert!(!line.starts_with("       "), "and no more: {line:?}");
            let (label, _) = line.trim_start().split_once(' ').unwrap_or((line, ""));
            assert!(label.ends_with(':'), "a `label:` prefix: {line:?}");
            assert!(label.len() <= 13, "fits the label column: {line:?}");
        }
    }

    #[test]
    fn render_table_aligns_columns() {
        let rows = vec![vec!["a".to_owned(), "bb".to_owned()]];
        let table = render_table(&["id", "kind"], &rows);
        let mut lines = table.lines();
        assert_eq!(lines.next(), Some("id  kind"));
        assert!(lines.next().unwrap().starts_with("--"));
        assert_eq!(lines.next(), Some("a   bb"));
    }
}
