//! Thin command wiring for `dujactl`: each function performs one command's I/O
//! and returns its process exit code. All parsing and formatting live in the
//! pure [`crate::cli`] and [`crate::fmt`] modules; the logic here is only the
//! backend calls and printing.

use duja_core::input_source;
use duja_core::model::Feature;
use duja_core::quirks::QuirkDb;

use crate::backend;
use crate::cli::{EXIT_BACKEND, EXIT_OK, EXIT_UNKNOWN_DISPLAY, EXIT_USAGE, SetTarget};
use crate::fmt;
use crate::fmt::{features_label, kind_label, pct_to_raw, raw_to_pct, render_table};
use crate::ipc;

/// Note, in verbose mode, that the direct backend served the request.
fn note_direct(verbose: bool) {
    if verbose {
        eprintln!("dujactl: no running app; served over direct backend");
    }
}

/// `list`: over IPC if the app is up, else the direct backend.
pub fn list(verbose: bool) -> u8 {
    if let Some(mut client) = ipc::try_connect() {
        return ipc::list(&mut client, verbose);
    }
    note_direct(verbose);
    list_direct()
}

/// `list` against the direct in-process backend.
fn list_direct() -> u8 {
    let displays = backend::discover();
    if displays.is_empty() {
        println!("no displays found");
        return EXIT_OK;
    }

    let rows: Vec<Vec<String>> = displays
        .iter()
        .map(|d| {
            let (brightness, features) = read_brightness_and_features(d.id.as_str());
            vec![
                d.id.as_str().to_owned(),
                kind_label(d.kind).to_owned(),
                d.name.clone(),
                brightness,
                features,
            ]
        })
        .collect();

    println!(
        "{}",
        render_table(&["id", "kind", "name", "brightness", "features"], &rows)
    );
    EXIT_OK
}

/// Open a controller and read `(current/max, features)` for the `list` table.
fn read_brightness_and_features(id: &str) -> (String, String) {
    let Some(mut controller) = backend::open(id) else {
        return ("?/?".to_owned(), "-".to_owned());
    };
    let features = controller
        .probe()
        .map_or_else(|_| "?".to_owned(), |caps| features_label(&caps));
    let brightness = match controller.get(Feature::Brightness) {
        Ok(range) => format!("{}/{}", range.current, range.max),
        Err(_) => "?/?".to_owned(),
    };
    (brightness, features)
}

/// `get <id>`: over IPC if the app is up, else the direct backend.
pub fn get(id: &str, verbose: bool) -> u8 {
    if let Some(mut client) = ipc::try_connect() {
        return ipc::get(&mut client, id, verbose);
    }
    note_direct(verbose);
    get_direct(id)
}

/// `get <id>` against the direct backend: print current/max and percent.
fn get_direct(id: &str) -> u8 {
    if !is_known(id) {
        eprintln!("unknown display `{id}`");
        return EXIT_UNKNOWN_DISPLAY;
    }
    let Some(mut controller) = backend::open(id) else {
        eprintln!("backend error: could not open display `{id}`");
        return EXIT_BACKEND;
    };
    match controller.get(Feature::Brightness) {
        Ok(range) => {
            println!(
                "{}/{} ({}%)",
                range.current,
                range.max,
                raw_to_pct(range.current, range.max)
            );
            EXIT_OK
        }
        Err(err) => {
            eprintln!("backend error reading `{id}`: {err}");
            EXIT_BACKEND
        }
    }
}

/// `set <id|all> brightness <0-100>`: over IPC if the app is up, else direct.
pub fn set(target: &SetTarget, percent: u8, verbose: bool) -> u8 {
    if let Some(mut client) = ipc::try_connect() {
        return ipc::set(&mut client, target, percent, verbose);
    }
    note_direct(verbose);
    set_direct(target, percent)
}

/// `set` against the direct backend: map the percent onto each display's probed
/// range, write, read back, and print the result.
fn set_direct(target: &SetTarget, percent: u8) -> u8 {
    let ids: Vec<String> = match target {
        SetTarget::All => backend::discover()
            .into_iter()
            .map(|d| d.id.as_str().to_owned())
            .collect(),
        SetTarget::One(id) => {
            if !is_known(id) {
                eprintln!("unknown display `{id}`");
                return EXIT_UNKNOWN_DISPLAY;
            }
            vec![id.clone()]
        }
    };

    if ids.is_empty() {
        println!("no displays found");
        return EXIT_OK;
    }

    let mut exit = EXIT_OK;
    for id in &ids {
        match apply_set(id, percent) {
            Ok(line) => println!("{line}"),
            Err(line) => {
                eprintln!("{line}");
                exit = EXIT_BACKEND;
            }
        }
    }
    exit
}

/// Perform the read-scale-write-verify cycle for one display.
fn apply_set(id: &str, percent: u8) -> Result<String, String> {
    let mut controller =
        backend::open(id).ok_or_else(|| format!("backend error: could not open display `{id}`"))?;
    let range = controller
        .get(Feature::Brightness)
        .map_err(|err| format!("backend error reading `{id}`: {err}"))?;
    let raw = pct_to_raw(percent, range.max);
    controller
        .set(Feature::Brightness, raw)
        .map_err(|err| format!("backend error writing `{id}`: {err}"))?;
    let after = controller
        .get(Feature::Brightness)
        .map_err(|err| format!("backend error verifying `{id}`: {err}"))?;
    Ok(format!(
        "{id}: set {percent}% -> {}/{} ({}%)",
        after.current,
        after.max,
        raw_to_pct(after.current, after.max)
    ))
}

/// `input <id> [<name|code>]`: list a display's allowed input sources (and the
/// current one), or switch to the requested input.
///
/// The allowed set is the display's probed
/// [`allowed_inputs`](duja_core::model::Capabilities::allowed_inputs): the
/// capability-string `0x60` value list intersected with any quirk override, and
/// empty when the display advertises no switchable inputs. A switch validates the
/// request against that set *before* writing, so `dujactl` never asks a monitor
/// to select an input it did not advertise.
///
/// There is no auto-revert: if a switch lands on a dead input, re-run
/// `dujactl input <id> <name>` from another machine/input to recover.
pub fn input(id: &str, value: Option<&str>) -> u8 {
    if !is_known(id) {
        eprintln!("unknown display `{id}`");
        return EXIT_UNKNOWN_DISPLAY;
    }
    let Some(mut controller) = backend::open(id) else {
        eprintln!("backend error: could not open display `{id}`");
        return EXIT_BACKEND;
    };
    let caps = match controller.probe() {
        Ok(caps) => caps,
        Err(err) => {
            eprintln!("backend error probing `{id}`: {err}");
            return EXIT_BACKEND;
        }
    };
    if caps.allowed_inputs.is_empty() {
        println!("{id}: no switchable input sources advertised");
        return EXIT_OK;
    }

    match value {
        None => {
            // Read the current input (untrusted metadata; best effort).
            let current = controller.get(Feature::InputSource).ok().map(|r| r.current);
            println!("allowed inputs for {id}:");
            for &code in &caps.allowed_inputs {
                let here = current.is_some_and(|cur| cur == u16::from(code));
                let marker = if here { "  <- current" } else { "" };
                println!("  {} ({:#04x}){marker}", input_source::label(code), code);
            }
            EXIT_OK
        }
        Some(raw) => {
            let Some(code) = input_source::parse_input(raw) else {
                eprintln!("invalid input `{raw}` (want a name like hdmi1/dp1 or a code like 0x11)");
                return EXIT_USAGE;
            };
            if !caps.allows_input(code) {
                let names: Vec<String> = caps
                    .allowed_inputs
                    .iter()
                    .map(|&c| input_source::label(c))
                    .collect();
                eprintln!(
                    "input {} ({:#04x}) is not allowed on `{id}`; allowed: {}",
                    input_source::label(code),
                    code,
                    names.join(", ")
                );
                return EXIT_USAGE;
            }
            match controller.set(Feature::InputSource, u16::from(code)) {
                Ok(()) => {
                    println!(
                        "{id}: switched input -> {} ({:#04x})",
                        input_source::label(code),
                        code
                    );
                    EXIT_OK
                }
                Err(err) => {
                    eprintln!("backend error switching input on `{id}`: {err}");
                    EXIT_BACKEND
                }
            }
        }
    }
}

/// `doctor [--report]`: environment / backend / quirk diagnostics. Always exit 0.
///
/// The two counts are derived from the same merged, deduplicated display set the
/// per-display listing below walks, so the summary and the detail can never
/// disagree — and a built-in panel the DDC backend also surfaces is counted as an
/// internal panel rather than an external monitor (see
/// [`backend::external_count`]).
///
/// # Two audiences, split by `--report`
///
/// Plain `doctor` is the **environment** check `README.md` and `docs/qa-checklist.md`
/// cite: does Duja see your displays, is the app up, what does Duja already
/// believe about this EDID. It is deliberately probe-free and so costs no DDC
/// traffic — but it still prints the identity header, because a diagnostic whose
/// build you need a flag to see is worse for every reader of it.
///
/// `--report` is the **monitor** report `CONTRIBUTING.md` and the monitor-quirk
/// issue template ask reporters to paste. It adds what the hardware itself
/// answered: the raw MCCS capability string, the probed features, the live range
/// and hardware-range verdict, and the allowed input set. That costs one
/// open + probe + read per display, which is exactly what the template promises
/// ("the monitor identity and probed capabilities") and what plain `doctor`
/// cannot carry: the `quirks:` line is `QuirkDb::resolve`, i.e. what Duja
/// *believes* about that EDID, not what the monitor *reported*.
///
/// A probe failure is a finding, not an error: "enumerates but DDC is dead" is
/// otherwise byte-identical to a healthy monitor here. It is printed and the exit
/// code stays [`EXIT_OK`], so the reporter has something to paste.
///
/// The output is deliberately **not** fenced: the template's textarea uses
/// `render: text`, which already wraps a paste in a code block.
pub fn doctor(report: bool) -> u8 {
    let displays = backend::discover();
    let reachable = ipc::server_reachable();
    for line in doctor_lines(report, reachable, &displays, &probe_display) {
        println!("{line}");
    }
    EXIT_OK
}

/// Assemble every line [`doctor`] prints.
///
/// The seam that makes the report testable at all: the two hardware/OS answers
/// (`displays`, `reachable`) are arguments, and `probe` — the only thing here that
/// can touch a monitor — is injected. `dujactl` has no fake-backend
/// infrastructure, so without this the whole feature was unpinnable: replacing the
/// body of the `--report` branch with `let _ = report;` left the entire suite
/// green (913/913), which is what the review of `#95` caught.
///
/// `probe` is called **once per display and only when `report` is set**. That is
/// the contract that keeps plain `doctor` free of DDC traffic, and it is asserted
/// by counting the calls rather than inferred from the output.
fn doctor_lines(
    report: bool,
    reachable: bool,
    displays: &[backend::CtlDisplay],
    probe: &impl Fn(&str) -> fmt::ProbeOutcome,
) -> Vec<String> {
    let mut lines = vec![
        "duja doctor".to_owned(),
        // `CARGO_PKG_VERSION`, not `duja_core::version()`: the label says
        // `dujactl`, so it must be the version of *this* binary. They are equal
        // only because both inherit `version.workspace = true`, and unlike every
        // library crate here `dujactl` has no `version()` of its own (nor a test
        // tying it to duja-core's).
        fmt::report_header(
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
        ),
        fmt::ipc_line(reachable),
        fmt::summary_line(
            "external monitors:",
            &backend::external_count(displays).to_string(),
        ),
        fmt::summary_line(
            "internal panels:",
            &backend::internal_count(displays).to_string(),
        ),
    ];

    if displays.is_empty() {
        lines.push(
            "  no displays visible — if you expect some, check you are in an interactive console session (qwinsta)"
                .to_owned(),
        );
        return lines;
    }

    lines.push(String::new());
    let db = QuirkDb::embedded();
    for d in displays {
        lines.extend(fmt::display_block(
            d.kind,
            d.id.as_str(),
            &d.name,
            &db.resolve(&d.id),
            report.then(|| probe(d.id.as_str())).as_ref(),
        ));
    }
    lines
}

/// Open, probe and read one display for `doctor --report`.
///
/// The only impure part of the report: the open/probe/read calls. Every decision
/// about what the answers *mean* lives in [`fmt::display_block`] and the pure
/// renderers under it. Both failure shapes — a display that will not open and one
/// that will not probe — come back as [`Err`] and are *printed as findings*, so
/// `doctor` keeps its "always exit 0" contract.
///
/// The controller is dropped on return, releasing its OS handle (a
/// physical-monitor `HANDLE` on Windows, an I2C service handle on macOS) before
/// the next display is opened.
fn probe_display(id: &str) -> fmt::ProbeOutcome {
    let mut controller =
        backend::open(id).ok_or_else(|| "could not open the display".to_owned())?;
    let caps = controller.probe().map_err(|err| err.to_string())?;
    let brightness = controller
        .get(Feature::Brightness)
        .ok()
        .map(|range| (range.current, range.max));
    Ok((caps, brightness))
}

/// `version`: print the workspace version.
pub fn version() -> u8 {
    println!("dujactl {}", duja_core::version());
    EXIT_OK
}

/// Whether a display with id string `id` is currently enumerated.
fn is_known(id: &str) -> bool {
    backend::discover().iter().any(|d| d.id.as_str() == id)
}

#[cfg(test)]
mod tests {
    use super::doctor_lines;
    use crate::backend::CtlDisplay;
    use crate::fmt::ProbeOutcome;
    use duja_core::id::StableDisplayId;
    use duja_core::model::{Capabilities, DisplayKind, Feature};
    use std::cell::Cell;

    /// One external monitor, as `backend::discover` would hand it over.
    fn external() -> CtlDisplay {
        CtlDisplay {
            id: StableDisplayId::from_parts("MSI", 0x30B6, Some("PB6H013202527")).unwrap(),
            kind: DisplayKind::ExternalDdc,
            name: "MSI MP273QP".to_owned(),
        }
    }

    /// A healthy probe answer: a real-ish capability string, a live range, and one
    /// switchable input.
    // RATIONALE: `unnecessary_wraps` — this is a `fn() -> ProbeOutcome` fixture
    // handed to `counting` alongside the `Err` fixture, so both must have the
    // outcome type; unwrapping it would make the two incompatible.
    #[allow(clippy::unnecessary_wraps)]
    fn healthy() -> ProbeOutcome {
        Ok((
            Capabilities {
                features: [Feature::Brightness, Feature::InputSource]
                    .into_iter()
                    .collect(),
                hardware_range: true,
                raw_capabilities: Some("(vcp(10 60(11 0F)))".to_owned()),
                allowed_inputs: vec![0x11],
            },
            Some((42, 100)),
        ))
    }

    /// A counting probe, so "was the hardware touched?" is asserted rather than
    /// inferred from the rendered text.
    fn counting(
        calls: &Cell<usize>,
        outcome: fn() -> ProbeOutcome,
    ) -> impl Fn(&str) -> ProbeOutcome {
        move |_id| {
            calls.set(calls.get().saturating_add(1));
            outcome()
        }
    }

    /// The whole point of `--report`, and the thing that had no coverage at all:
    /// the flag must actually *produce* the monitor detail. Before this seam
    /// existed, replacing the feature's body with `let _ = report;` left the suite
    /// green at 913/913.
    #[test]
    fn report_probes_each_display_and_prints_what_it_answered() {
        let calls = Cell::new(0);
        let out = doctor_lines(true, false, &[external()], &counting(&calls, healthy)).join("\n");

        assert_eq!(calls.get(), 1, "exactly one probe for one display");
        assert!(
            out.contains("(vcp(10 60(11 0F)))"),
            "the raw MCCS string: {out}"
        );
        assert!(out.contains("42/100 (42%)"), "the live range: {out}");
        assert!(
            out.contains("hw-range:"),
            "the hardware-range verdict: {out}"
        );
        assert!(out.contains("hdmi1 (0x11)"), "the allowed inputs: {out}");
        assert!(
            out.contains("brightness,input"),
            "the probed features: {out}"
        );
    }

    /// The other half of the contract: plain `doctor` must stay probe-free. It is
    /// the command `README.md` and `docs/qa-checklist.md` cite as *the* environment
    /// check, and a diagnostic that opens every monitor and pushes DDC traffic is a
    /// different, more intrusive command than the one those docs describe.
    #[test]
    fn plain_doctor_never_touches_a_display() {
        let calls = Cell::new(0);
        let out = doctor_lines(false, false, &[external()], &counting(&calls, healthy)).join("\n");

        assert_eq!(calls.get(), 0, "no probe, so no DDC traffic: {out}");
        assert!(
            out.contains("MSI MP273QP"),
            "the display is still listed: {out}"
        );
        assert!(
            out.contains("quirks:"),
            "with what Duja believes about it: {out}"
        );
        assert!(
            !out.contains("caps:"),
            "but nothing it did not ask for: {out}"
        );
    }

    /// A probe failure is the most valuable thing a quirk report can carry, and
    /// "enumerates but DDC is dead" used to be byte-identical to a healthy monitor
    /// in this output. It is a printed finding, and `doctor` still succeeds.
    #[test]
    fn a_dead_display_is_reported_as_a_finding_not_swallowed() {
        let calls = Cell::new(0);
        let out = doctor_lines(
            true,
            false,
            &[external()],
            &counting(&calls, || Err("Timeout".to_owned())),
        )
        .join("\n");

        assert_eq!(calls.get(), 1);
        assert!(out.contains("probe:") && out.contains("FAILED"), "{out}");
        assert!(out.contains("Timeout"), "the backend's own reason: {out}");
        // Distinguishable from the healthy render, which was the whole defect.
        let healthy_out = doctor_lines(
            true,
            false,
            &[external()],
            &counting(&Cell::new(0), healthy),
        )
        .join("\n");
        assert_ne!(out, healthy_out);
    }

    /// The identity header prints in **both** modes: `README.md`,
    /// `docs/qa-checklist.md` and `docs/STATUS.md` all cite plain `dujactl doctor`
    /// as the diagnostic, and a build number you need a flag to see is worse for
    /// every one of those readers. It leaks nothing.
    #[test]
    fn the_identity_header_prints_with_or_without_the_flag() {
        let calls = Cell::new(0);
        for report in [false, true] {
            let out =
                doctor_lines(report, true, &[external()], &counting(&calls, healthy)).join("\n");
            assert!(out.contains("dujactl:"), "report={report}: {out}");
            assert!(
                out.contains(env!("CARGO_PKG_VERSION")),
                "report={report}: {out}"
            );
            assert!(out.contains(std::env::consts::OS), "report={report}: {out}");
        }
    }

    /// With no displays at all there is nothing to probe, so the flag must not
    /// change anything: the header, the counts and the console-session hint are the
    /// whole report either way.
    #[test]
    fn an_empty_display_set_reports_the_console_session_hint_in_both_modes() {
        let calls = Cell::new(0);
        for report in [false, true] {
            let out = doctor_lines(report, false, &[], &counting(&calls, healthy)).join("\n");
            assert!(out.contains("no displays visible"), "{out}");
            assert!(out.contains("external monitors: 0"), "{out}");
            assert!(out.contains("internal panels:   0"), "{out}");
        }
        assert_eq!(calls.get(), 0, "nothing to probe");
    }
}
