//! Thin command wiring for `dujactl`: each function performs one command's I/O
//! and returns its process exit code. All parsing and formatting live in the
//! pure [`crate::cli`] and [`crate::fmt`] modules; the logic here is only the
//! backend calls and printing.

use duja_core::input_source;
use duja_core::model::Feature;
use duja_core::quirks::QuirkDb;

use crate::backend;
use crate::cli::{EXIT_BACKEND, EXIT_OK, EXIT_UNKNOWN_DISPLAY, EXIT_USAGE, SetTarget};
use crate::fmt::{features_label, kind_label, pct_to_raw, quirk_summary, raw_to_pct, render_table};

/// `list`: enumerate and print a table of displays.
pub fn list() -> u8 {
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

/// `get <id>`: print one display's brightness current/max and percent.
pub fn get(id: &str) -> u8 {
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

/// `set <id|all> brightness <0-100>`: map the percent onto each display's
/// probed range, write, read back, and print the result.
pub fn set(target: &SetTarget, percent: u8) -> u8 {
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

/// `doctor`: environment / backend / quirk diagnostics. Always exit 0.
pub fn doctor() -> u8 {
    let ddc = backend::ddc_count();
    let panel = backend::panel_count();

    println!("duja doctor");
    println!("  ddc displays:   {ddc}");
    println!("  panel displays: {panel}");
    if ddc == 0 && panel == 0 {
        println!(
            "  no displays visible — if you expect some, check you are in an interactive console session (qwinsta)"
        );
    }

    let displays = backend::discover();
    if !displays.is_empty() {
        println!();
        let db = QuirkDb::embedded();
        for d in &displays {
            let quirks = db.resolve(&d.id);
            println!("  [{}] {} ({})", kind_label(d.kind), d.id.as_str(), d.name);
            println!("      edid id: {}", d.id.as_str());
            println!("      quirks:  {}", quirk_summary(&quirks));
        }
    }
    EXIT_OK
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
