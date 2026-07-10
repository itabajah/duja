//! Pure formatting and brightness-scaling helpers for `dujactl`.
//!
//! Kept free of I/O and hardware so they are unit testable in isolation.

use duja_core::model::{Capabilities, DisplayKind, Feature};
use duja_core::quirks::ResolvedQuirks;

/// A short, stable label for a [`DisplayKind`].
pub fn kind_label(kind: DisplayKind) -> &'static str {
    match kind {
        DisplayKind::ExternalDdc => "ddc",
        DisplayKind::InternalPanel => "panel",
        DisplayKind::SoftwareOnly => "software",
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
    use super::{features_label, kind_label, pct_to_raw, quirk_summary, raw_to_pct, render_table};
    use duja_core::model::{Capabilities, DisplayKind, Feature};
    use duja_core::quirks::ResolvedQuirks;

    #[test]
    fn kind_labels_are_stable() {
        assert_eq!(kind_label(DisplayKind::ExternalDdc), "ddc");
        assert_eq!(kind_label(DisplayKind::InternalPanel), "panel");
        assert_eq!(kind_label(DisplayKind::SoftwareOnly), "software");
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
