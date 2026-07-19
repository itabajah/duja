//! Pure text formatting helpers: display-kind labels, feature lists, and a
//! simple aligned table renderer. Kept pure so they are unit testable.

use duja_core::model::{Capabilities, DisplayKind, Feature};

/// A short, stable label for a [`DisplayKind`] (its physical provenance).
pub(crate) fn kind_label(kind: DisplayKind) -> &'static str {
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
pub(crate) fn features_label(caps: &Capabilities) -> String {
    if caps.features.is_empty() {
        return "-".to_owned();
    }
    caps.features
        .iter()
        .map(|f| feature_label(*f))
        .collect::<Vec<_>>()
        .join(",")
}

/// Render an aligned text table: a header row, a dashed rule, then the rows.
/// Columns are padded to the widest cell. Never panics; extra/short rows are
/// tolerated by the zip.
pub(crate) fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
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
    use super::{features_label, kind_label, render_table};
    use duja_core::model::{Capabilities, DisplayKind, Feature};

    #[test]
    fn kind_labels_are_stable() {
        assert_eq!(kind_label(DisplayKind::ExternalDdc), "external");
        assert_eq!(kind_label(DisplayKind::InternalPanel), "internal");
    }

    #[test]
    fn features_label_lists_or_dashes() {
        let empty = Capabilities::default();
        assert_eq!(features_label(&empty), "-");

        let caps = Capabilities {
            features: [Feature::Brightness, Feature::Contrast]
                .into_iter()
                .collect(),
            hardware_range: true,
            raw_capabilities: None,
            allowed_inputs: Vec::new(),
        };
        assert_eq!(features_label(&caps), "brightness,contrast");
    }

    #[test]
    fn render_table_aligns_columns() {
        let rows = vec![
            vec!["GSM-1".to_owned(), "ddc".to_owned(), "Left".to_owned()],
            vec![
                "APP-22".to_owned(),
                "panel".to_owned(),
                "Internal".to_owned(),
            ],
        ];
        let table = render_table(&["id", "kind", "name"], &rows);
        let mut lines = table.lines();
        assert_eq!(lines.next(), Some("id      kind   name"));
        // second line is the dashed rule
        assert!(lines.next().unwrap().starts_with("--"));
        assert_eq!(lines.next(), Some("GSM-1   ddc    Left"));
        assert_eq!(lines.next(), Some("APP-22  panel  Internal"));
    }
}
