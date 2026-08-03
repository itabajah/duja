//! The Linux half of `dujactl doctor`: why the displays you expected are not
//! there.
//!
//! On Windows and macOS an empty display list is nearly always a real absence.
//! On Linux it is usually a **configuration** the user can fix and cannot see:
//! the `i2c-dev` module is not loaded, or `/dev/i2c-*` is root-only because no
//! package has installed a udev rule for it. `duja-ddc` skips those connectors
//! silently — correctly, since an unopenable bus is not a monitor — so without
//! this section a correctly-cabled machine with three monitors reports zero and
//! says nothing about why.
//!
//! Everything here is a **pure function over plain data**: the gathering lives in
//! [`crate::run`] behind a `cfg`, and this module turns the result into lines.
//! That is what lets the whole report be asserted on all three CI lanes, which
//! matters because the machine that would exercise it for real is the one this
//! project does not have.

use crate::fmt;

/// What happened when Duja tried to reach one DRM connector's DDC/CI channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum I2cState {
    /// A usable bus at `/dev/i2c-<n>`.
    Usable(u32),
    /// This is a built-in panel. DDC/CI does not reach one; the backlight does.
    BuiltInPanel,
    /// The graphics driver publishes no I2C adapter for this connector. Not
    /// user-fixable.
    NoAdapter,
    /// An adapter exists but `i2c-dev` is not loaded, so no `/dev/i2c-*` node
    /// exists for it. One `modprobe` away.
    NoI2cDev,
    /// The node exists and could not be opened. Carries the adapter number and
    /// the OS error text.
    Unopenable(u32, String),
}

/// One connector's row in the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectorRow {
    /// `DP-1`, `eDP-1`, `HDMI-A-2` — the DRM connector name, prefix stripped.
    pub name: String,
    /// What Duja found.
    pub state: I2cState,
}

/// Whether software dimming is available, per mechanism, as already decided by
/// `duja-dimmer`'s capability probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DimmingRow {
    /// `wayland`, `x11`, or `none`.
    pub transport: String,
    /// `None` when the overlay is available; otherwise the reason it is not.
    pub overlay: Option<String>,
    /// `None` when gamma is available; otherwise the reason it is not.
    pub gamma: Option<String>,
}

/// Everything the Linux section prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinuxReport {
    /// Every connected DRM connector, in enumeration order.
    pub connectors: Vec<ConnectorRow>,
    /// The software-dimming capability report.
    pub dimming: DimmingRow,
}

/// The remedy for a state, or `None` when there is nothing the user can do.
///
/// Separated from the state's *description* because these are the only lines in
/// `doctor` that ask the user to run something, and a wrong one wastes their
/// time. `NoAdapter` deliberately has none: the driver not publishing an adapter
/// is not a configuration.
fn remedy(state: &I2cState) -> Option<&'static str> {
    match state {
        I2cState::NoI2cDev => Some("sudo modprobe i2c-dev (and add it to /etc/modules-load.d)"),
        // The `i2c` group does not exist until a package creates it, so "join the
        // group" alone is advice a stock system cannot follow.
        I2cState::Unopenable(..) => Some(
            "install i2c-tools (or ddcutil) for the udev rule, then add yourself to the i2c group",
        ),
        I2cState::Usable(_) | I2cState::BuiltInPanel | I2cState::NoAdapter => None,
    }
}

/// Describe a state in one phrase.
fn describe(state: &I2cState) -> String {
    match state {
        I2cState::Usable(bus) => format!("ok, /dev/i2c-{bus}"),
        I2cState::BuiltInPanel => "built-in panel (driven by its backlight, not DDC/CI)".to_owned(),
        I2cState::NoAdapter => "the graphics driver publishes no I2C adapter for it".to_owned(),
        I2cState::NoI2cDev => "the i2c-dev module is not loaded".to_owned(),
        I2cState::Unopenable(bus, err) => format!("/dev/i2c-{bus} could not be opened: {err}"),
    }
}

/// Render the Linux section.
///
/// Returns an empty list when there is nothing to say, so the caller can splice
/// it in unconditionally.
pub(crate) fn lines(report: &LinuxReport) -> Vec<String> {
    let mut out = vec![String::new(), "linux diagnostics".to_owned()];

    if report.connectors.is_empty() {
        out.push(
            "  no DRM connectors are connected — if a monitor is plugged in, /sys/class/drm is not \
             reporting it"
                .to_owned(),
        );
    } else {
        out.push("  DRM connectors:".to_owned());
        for row in &report.connectors {
            out.push(format!("    {}: {}", row.name, describe(&row.state)));
            if let Some(fix) = remedy(&row.state) {
                out.push(format!("      try: {fix}"));
            }
        }
    }

    out.push(format!("  display server: {}", report.dimming.transport));
    out.push(format!(
        "  overlay dimming: {}",
        availability(report.dimming.overlay.as_deref())
    ));
    out.push(format!(
        "  gamma dimming: {}",
        availability(report.dimming.gamma.as_deref())
    ));
    out
}

/// `available`, or `unavailable` with the reason.
fn availability(reason: Option<&str>) -> String {
    match reason {
        None => "available".to_owned(),
        Some(reason) => format!("unavailable ({reason})"),
    }
}

/// The summary counts the report adds to `doctor`'s header.
///
/// Kept apart from [`lines`] because these belong beside the existing
/// `external monitors:` / `internal panels:` counts rather than at the bottom.
pub(crate) fn summary(report: &LinuxReport) -> Vec<String> {
    let usable = report
        .connectors
        .iter()
        .filter(|row| matches!(row.state, I2cState::Usable(_)))
        .count();
    let blocked = report
        .connectors
        .iter()
        .filter(|row| matches!(row.state, I2cState::NoI2cDev | I2cState::Unopenable(..)))
        .count();
    let mut out = vec![fmt::summary_line(
        "DDC-capable connectors:",
        &usable.to_string(),
    )];
    if blocked > 0 {
        // Only when it is non-zero: a line reading "blocked: 0" on a healthy
        // machine invites a user to go looking for a problem they do not have.
        out.push(fmt::summary_line(
            "blocked by configuration:",
            &blocked.to_string(),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, state: I2cState) -> ConnectorRow {
        ConnectorRow {
            name: name.to_owned(),
            state,
        }
    }

    fn dimming(transport: &str) -> DimmingRow {
        DimmingRow {
            transport: transport.to_owned(),
            overlay: None,
            gamma: None,
        }
    }

    fn joined(report: &LinuxReport) -> String {
        lines(report).join("\n")
    }

    /// The whole reason this section exists: a machine where every monitor is
    /// cabled and none is reachable must say **why**, and must say what to run.
    #[test]
    fn a_blocked_machine_names_the_cause_and_the_remedy() {
        let report = LinuxReport {
            connectors: vec![
                row("DP-1", I2cState::NoI2cDev),
                row("HDMI-A-1", I2cState::NoI2cDev),
            ],
            dimming: dimming("x11"),
        };

        let text = joined(&report);

        assert!(text.contains("the i2c-dev module is not loaded"), "{text}");
        assert!(text.contains("sudo modprobe i2c-dev"), "{text}");
        // Both connectors are reported, not just the first.
        assert!(text.contains("DP-1"), "{text}");
        assert!(text.contains("HDMI-A-1"), "{text}");
    }

    /// The permission remedy must not be the usual "join the `i2c` group": that
    /// group does not exist until a package creates it, so on a stock system the
    /// advice cannot be followed. This is the single most likely line in the
    /// whole report to be read by a stuck user.
    #[test]
    fn the_permission_remedy_names_the_package_and_not_only_the_group() {
        let report = LinuxReport {
            connectors: vec![row(
                "DP-1",
                I2cState::Unopenable(7, "Permission denied (os error 13)".to_owned()),
            )],
            dimming: dimming("wayland"),
        };

        let text = joined(&report);

        assert!(text.contains("/dev/i2c-7 could not be opened"), "{text}");
        assert!(text.contains("Permission denied"), "{text}");
        assert!(text.contains("i2c-tools"), "{text}");
        assert!(text.contains("i2c group"), "{text}");
    }

    /// A state the user cannot act on must not be given a remedy. An instruction
    /// that cannot work is worse than none: it sends someone to reconfigure a
    /// machine that is already correct.
    #[test]
    fn states_with_no_remedy_are_not_given_one() {
        for state in [
            I2cState::Usable(3),
            I2cState::BuiltInPanel,
            I2cState::NoAdapter,
        ] {
            let report = LinuxReport {
                connectors: vec![row("X-1", state.clone())],
                dimming: dimming("x11"),
            };
            let text = joined(&report);
            assert!(
                !text.contains("try:"),
                "{state:?} was given a remedy: {text}"
            );
        }
    }

    /// An internal panel is not a fault and must not read as one — it is the
    /// expected state of every laptop, and DDC/CI cannot reach one by design.
    #[test]
    fn a_built_in_panel_reads_as_expected_rather_than_broken() {
        let report = LinuxReport {
            connectors: vec![row("eDP-1", I2cState::BuiltInPanel)],
            dimming: dimming("wayland"),
        };

        let text = joined(&report);

        assert!(text.contains("built-in panel"), "{text}");
        assert!(text.contains("backlight"), "{text}");
    }

    #[test]
    fn a_working_machine_reports_the_bus_it_found() {
        let report = LinuxReport {
            connectors: vec![row("DP-1", I2cState::Usable(7))],
            dimming: dimming("x11"),
        };

        assert!(joined(&report).contains("ok, /dev/i2c-7"));
    }

    /// The dimming lines carry the probe's reason verbatim, because that reason
    /// is the one thing ADR-0011 exists to produce: a per-session answer that
    /// names an interface rather than a desktop.
    #[test]
    fn unavailable_dimming_carries_the_probes_reason() {
        let report = LinuxReport {
            connectors: vec![],
            dimming: DimmingRow {
                transport: "wayland".to_owned(),
                overlay: Some("the compositor does not offer zwlr_layer_shell_v1".to_owned()),
                gamma: Some("another client holds it".to_owned()),
            },
        };

        let text = joined(&report);

        assert!(text.contains("overlay dimming: unavailable"), "{text}");
        assert!(text.contains("zwlr_layer_shell_v1"), "{text}");
        assert!(text.contains("gamma dimming: unavailable"), "{text}");
        assert!(text.contains("another client holds it"), "{text}");
    }

    #[test]
    fn available_dimming_says_so_without_a_reason() {
        let report = LinuxReport {
            connectors: vec![],
            dimming: dimming("x11"),
        };

        let text = joined(&report);

        assert!(text.contains("overlay dimming: available"), "{text}");
        assert!(!text.contains("unavailable"), "{text}");
    }

    /// No connectors at all is a distinct state from connectors that cannot be
    /// opened, and conflating them would send a user with a cabling problem after
    /// a permissions one.
    #[test]
    fn no_connectors_is_its_own_message() {
        let report = LinuxReport {
            connectors: vec![],
            dimming: dimming("none"),
        };

        let text = joined(&report);

        assert!(text.contains("no DRM connectors are connected"), "{text}");
        assert!(!text.contains("DRM connectors:"), "{text}");
    }

    /// The blocked count only appears when it is non-zero: a "blocked: 0" line on
    /// a healthy machine invites a hunt for a problem that is not there.
    #[test]
    fn the_blocked_count_is_omitted_when_nothing_is_blocked() {
        let healthy = LinuxReport {
            connectors: vec![row("DP-1", I2cState::Usable(7))],
            dimming: dimming("x11"),
        };
        let healthy_summary = summary(&healthy).join("\n");
        assert!(
            healthy_summary.contains("DDC-capable connectors:"),
            "{healthy_summary}"
        );
        assert!(!healthy_summary.contains("blocked"), "{healthy_summary}");

        let blocked = LinuxReport {
            connectors: vec![
                row("DP-1", I2cState::NoI2cDev),
                row("DP-2", I2cState::Unopenable(9, "denied".to_owned())),
                row("eDP-1", I2cState::BuiltInPanel),
            ],
            dimming: dimming("x11"),
        };
        let blocked_summary = summary(&blocked).join("\n");
        assert!(
            blocked_summary.contains("blocked by configuration:"),
            "{blocked_summary}"
        );
        // The built-in panel is not "blocked" — it is not a DDC/CI candidate at
        // all, and counting it would overstate the problem.
        assert!(blocked_summary.contains('2'), "{blocked_summary}");
    }
}
