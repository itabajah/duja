//! `cargo xtask size` — the stripped-binary budget, enforced rather than
//! remembered.
//!
//! [`ADR-0012`] raised the budget to 16 MB at the P4 gate and recorded that P5
//! had already blown through it. Between those two gates nothing measured the
//! binary on the way past: it went from 14.9 MB to 19.4 MB across two releases,
//! and what noticed was a human reading a ledger months later. A budget that is
//! only checked when somebody remembers to check it is a budget that drifts, so
//! P8 wave 1 recovered the bytes **and** put this in the path of a release.
//!
//! # Where it runs, and the gap that leaves
//!
//! The release workflow calls it after building the release binaries, so a
//! release cannot ship over budget. It deliberately does **not** run on every
//! PR: the check needs a `--release` build with fat LTO, which is roughly twenty
//! minutes on a hosted Windows runner, and paying that on every pull request to
//! catch a regression that arrives once a year is the wrong trade.
//!
//! The gap that leaves is real and worth naming rather than implying it away: a
//! dependency bump that adds a megabyte is caught at the *next release*, not at
//! the PR that lands it. `docs/debt.md` carries it.
//!
//! # Why bytes rather than megabytes
//!
//! Because "16 MB" was ambiguous for four gates and nobody noticed. The ledger
//! rows read 14.9 and 17.21 with no unit named anywhere, and 16 MB (16,000,000)
//! and 16 MiB (16,777,216) differ by more than some of the levers that were
//! being weighed against them. The budget here is an integer number of bytes and
//! the human-readable figure is derived from it, never the other way round.
//!
//! [`ADR-0012`]: ../../../docs/adr/0012-binary-size-budget-variance.md

use std::path::{Path, PathBuf};

use crate::bundle::{HELPER_EXECUTABLE, MAIN_EXECUTABLE};
use crate::repo_root;

/// The stripped-release budget for `duja`, in bytes.
///
/// 16 MiB exactly. [`ADR-0012`](self) set "16 MB" without a unit; P8 wave 1 had
/// to pick one, and picked the larger of the two readings **on purpose**: the
/// measured binary lands under both, so choosing the loose reading costs nothing
/// today and the alternative would be quietly tightening a budget under cover of
/// disambiguating it.
pub(crate) const MAIN_BUDGET_BYTES: u64 = 16 * 1024 * 1024;

/// The stripped-release budget for `dujactl`, in bytes.
///
/// 2 MiB. `docs/perf-budgets.md` never set one for the helper — it recorded 0.6
/// MB as an observation — and an unbudgeted binary is one nobody would notice
/// growing. This is deliberately loose: `dujactl` measures 0.79 MiB, so the
/// ceiling is not a target to grow into but a tripwire for the change that
/// accidentally links the GUI stack into the CLI.
pub(crate) const HELPER_BUDGET_BYTES: u64 = 2 * 1024 * 1024;

/// One binary measured against its budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Measured {
    /// The executable's name, without any platform extension.
    pub(crate) name: String,
    /// What it measured, in bytes.
    pub(crate) bytes: u64,
    /// What it is allowed to be, in bytes.
    pub(crate) budget: u64,
}

impl Measured {
    /// Whether this binary is within its budget. Equality passes: a budget is a
    /// ceiling, and a binary that lands exactly on it has not exceeded it.
    pub(crate) const fn within(&self) -> bool {
        self.bytes <= self.budget
    }

    /// How far over budget, in bytes. Zero when within.
    pub(crate) const fn over_by(&self) -> u64 {
        self.bytes.saturating_sub(self.budget)
    }

    /// The one-line report for this binary.
    pub(crate) fn line(&self) -> String {
        let verdict = if self.within() {
            format!(
                "ok, {} bytes to spare",
                self.budget.saturating_sub(self.bytes)
            )
        } else {
            format!("OVER BUDGET by {} bytes", self.over_by())
        };
        format!(
            "{:<10} {:>12} bytes ({:>6.2} MiB)  budget {:>12} ({:>6.2} MiB)  {verdict}",
            self.name,
            self.bytes,
            mib(self.bytes),
            self.budget,
            mib(self.budget),
        )
    }
}

/// Bytes as MiB, for display only. Never the basis of a comparison — see the
/// module header on why the budget is an integer number of bytes.
// RATIONALE: display only, and never the basis of a comparison. An f64 holds
// every u64 up to 2^53 exactly; a binary large enough to lose precision here has
// problems this function cannot express.
#[allow(clippy::cast_precision_loss)]
fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// `cargo xtask size [--target <triple>]`.
///
/// # Errors
/// Returns a message when an argument is unrecognised, when a binary is missing
/// (with the `cargo build` line that would produce it), or when any binary is
/// over budget.
pub(crate) fn run(mut args: std::env::Args) -> Result<(), String> {
    let mut triple: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--target" => {
                triple = Some(
                    args.next()
                        .ok_or_else(|| "--target needs a target triple".to_owned())?,
                );
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }

    let root = repo_root()?;
    let dir = release_dir(&root, triple.as_deref());
    let measured = measure_all(&dir)?;

    for one in &measured {
        println!("{}", one.line());
    }

    let over: Vec<&Measured> = measured.iter().filter(|m| !m.within()).collect();
    if over.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} binary/binaries over budget. This is ADR-0012's ledger, not a \
         suggestion: either recover the bytes or supersede the ADR with an \
         explicit raise-with-rationale, which is what it asks for.",
        over.len()
    ))
}

/// Where `cargo build --release` puts binaries, with or without an explicit
/// `--target`.
///
/// Cargo moves the output under `target/<triple>/release` the moment a target is
/// named, *even when the triple is the host's*. Getting this wrong measures a
/// stale binary from a previous build rather than failing, which is the one
/// failure mode a size check must not have.
fn release_dir(root: &Path, triple: Option<&str>) -> PathBuf {
    let target = root.join("target");
    match triple {
        Some(triple) => target.join(triple).join("release"),
        None => target.join("release"),
    }
}

/// Measure both shipped binaries in `dir`.
fn measure_all(dir: &Path) -> Result<Vec<Measured>, String> {
    [
        (MAIN_EXECUTABLE, MAIN_BUDGET_BYTES),
        (HELPER_EXECUTABLE, HELPER_BUDGET_BYTES),
    ]
    .into_iter()
    .map(|(name, budget)| measure(dir, name, budget))
    .collect()
}

/// Measure one binary, trying the bare name and then `.exe`.
fn measure(dir: &Path, name: &str, budget: u64) -> Result<Measured, String> {
    let bare = dir.join(name);
    let exe = dir.join(format!("{name}.exe"));
    let path = if bare.is_file() {
        bare
    } else if exe.is_file() {
        exe
    } else {
        return Err(format!(
            "missing {}: run `cargo build --release -p duja-app -p dujactl` first",
            bare.display()
        ));
    };
    let bytes = std::fs::metadata(&path)
        .map_err(|e| format!("reading {}: {e}", path.display()))?
        .len();
    Ok(Measured {
        name: name.to_owned(),
        bytes,
        budget,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binary_on_its_budget_exactly_is_within_it() {
        let on_the_line = Measured {
            name: "duja".to_owned(),
            bytes: MAIN_BUDGET_BYTES,
            budget: MAIN_BUDGET_BYTES,
        };
        assert!(on_the_line.within());
        assert_eq!(on_the_line.over_by(), 0);
    }

    #[test]
    fn one_byte_over_is_over() {
        let over = Measured {
            name: "duja".to_owned(),
            bytes: MAIN_BUDGET_BYTES.saturating_add(1),
            budget: MAIN_BUDGET_BYTES,
        };
        assert!(!over.within());
        assert_eq!(over.over_by(), 1);
        assert!(over.line().contains("OVER BUDGET by 1 bytes"));
    }

    /// The failure this check exists to prevent, as a fixture: the P5 binary
    /// against the P4 budget. Nothing measured it at the time, and the overage
    /// was found by reading a ledger months later.
    #[test]
    fn the_p5_regression_would_have_failed_this_check() {
        let p5 = Measured {
            name: "duja".to_owned(),
            // 17.21 MB as ADR-0012's ledger recorded it, read as MiB - the
            // reading that is *kindest* to the binary, and it fails anyway.
            bytes: 18_046_546,
            budget: MAIN_BUDGET_BYTES,
        };
        assert!(!p5.within(), "{}", p5.line());
    }

    /// Naming a target moves the output directory, including when the triple is
    /// the host's own. A check that looked in `target/release` after a
    /// `--target x86_64-pc-windows-msvc` build would measure whatever was left
    /// there by an earlier build and report a stale pass.
    #[test]
    fn naming_a_target_moves_where_the_binaries_are_looked_for() {
        let root = Path::new("/repo");
        assert_eq!(release_dir(root, None), Path::new("/repo/target/release"));
        assert_eq!(
            release_dir(root, Some("x86_64-pc-windows-msvc")),
            Path::new("/repo/target/x86_64-pc-windows-msvc/release")
        );
    }

    #[test]
    fn a_missing_binary_names_the_command_that_would_build_it() {
        let err = measure(Path::new("/nonexistent"), "duja", MAIN_BUDGET_BYTES)
            .expect_err("a missing binary must not measure");
        assert!(err.contains("cargo build --release"), "{err}");
    }

    /// The budget constants here and the figures in `docs/perf-budgets.md` are
    /// one decision written in two places, and the doc is what a human reads
    /// before deciding whether a change is affordable. Restating a number in
    /// prose is what rots; reading the other file is what does not.
    #[test]
    fn the_budget_table_in_perf_budgets_agrees_with_these_constants() {
        // The doc writes `16,777,216` because a human reads it; the constant is
        // an integer because a comparison uses it. Strip the separators rather
        // than making either side write the other's format.
        let doc = crate::read_repo_file(&["docs", "perf-budgets.md"]).replace(',', "");
        for (label, budget) in [
            ("`duja`", MAIN_BUDGET_BYTES),
            ("`dujactl`", HELPER_BUDGET_BYTES),
        ] {
            let needle = format!("{budget} bytes");
            assert!(
                doc.contains(&needle),
                "docs/perf-budgets.md does not state the {label} budget as \
                 `{needle}`. The budget is bytes here and must be bytes there: \
                 the whole reason this constant exists is that four gates \
                 argued past each other over whether `16 MB` meant 16,000,000 \
                 or 16,777,216."
            );
        }
    }
}
