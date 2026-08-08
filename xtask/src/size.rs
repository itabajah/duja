//! `cargo xtask size` — the stripped-binary budget, enforced rather than
//! remembered.
//!
//! [ADR-0012] raised the budget to 16 MB at the P4 gate and recorded that P5
//! had already blown through it. Between those two gates nothing measured the
//! binary on the way past: it went from 14.9 MB to 19.4 MB across two releases,
//! and what noticed was a human reading a ledger months later. A budget that is
//! only checked when somebody remembers to check it is a budget that drifts, so
//! P8 wave 1 recovered the bytes **and** put this in the path of a release.
//!
//! # Where it runs, and the two gaps that leaves
//!
//! The release workflow's **Windows** job calls it after building, so a Windows
//! release cannot ship over budget. Two things it does not cover, both real:
//!
//! - **The other two platforms.** The `macos` and `linux` jobs build their own
//!   binaries and neither calls this. macOS especially cannot use the same
//!   number - its artifact is a `lipo` universal binary carrying two
//!   architectures, so a single-arch ceiling is the wrong shape, and no budget
//!   has ever been measured for either. Gating them on a number nobody has
//!   measured would be a worse failure than not gating them.
//! - **Pull requests.** The check needs a `--release` build with fat LTO,
//!   roughly twenty minutes on a hosted Windows runner. A dependency bump that
//!   adds a megabyte is caught at the *next release*, not at the PR that lands
//!   it.
//!
//! Both are `docs/debt.md` D-110 rather than unstated.
//!
//! # Why bytes rather than megabytes
//!
//! Because "16 MB" was ambiguous for four gates and nobody noticed. The ledger
//! rows read 14.9 and 17.21 with no unit named anywhere, and 16 MB (16,000,000)
//! and 16 MiB (16,777,216) differ by more than some of the levers that were
//! being weighed against them. The budget here is an integer number of bytes and
//! the human-readable figure is derived from it, never the other way round.
//!
//! [ADR-0012]: https://github.com/itabajah/duja/blob/main/docs/adr/0012-binary-size-budget-variance.md

use std::path::{Path, PathBuf};

use crate::bundle::{HELPER_EXECUTABLE, MAIN_EXECUTABLE};
use crate::repo_root;

/// The stripped-release budget for `duja`, in bytes.
///
/// 16 MiB exactly. ADR-0012 set "16 MB" without a unit; P8 wave 1 had
/// to pick one, and picked the larger of the two readings **on purpose**: the
/// measured binary lands under both, so choosing the loose reading costs nothing
/// today and the alternative would be quietly tightening a budget under cover of
/// disambiguating it.
pub(crate) const MAIN_BUDGET_BYTES: u64 = 16 * 1024 * 1024;

/// The stripped-release budget for `dujactl`, in bytes.
///
/// 2 MiB. Nothing set one for the helper before P8: `docs/perf-budgets.md` did
/// not mention `dujactl` at all, and the 0.6 MB figure people quote is an
/// *observation* in ADR-0012 rather than a budget. An unbudgeted binary is one
/// nobody would notice growing. This is deliberately loose - `dujactl` measures
/// 643,584 bytes - so the ceiling is not a target to grow into but a tripwire
/// for the change that accidentally links the GUI stack into the CLI.
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

/// One resolved `size` command line.
///
/// Parsing is separated from measuring for the reason [`dist`](crate::dist)
/// separated its own: a test can obtain an `std::env::Args` but cannot choose
/// what is *in* one, so every argument rule below was unreachable by
/// construction rather than by omission. `docs/debt-archive.md` D-114 has the
/// measurement.
#[derive(Debug, PartialEq, Eq)]
struct Invocation {
    /// The target triple from `--target`, if one was named.
    triple: Option<String>,
}

impl Invocation {
    /// Parse the arguments following `size`.
    ///
    /// # Errors
    /// A message for an unknown argument, or for a `--target` whose value is
    /// missing, empty, or itself flag-shaped.
    fn parse<I: Iterator<Item = String>>(mut args: I) -> Result<Invocation, String> {
        let mut triple: Option<String> = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                // Last one wins, which is the GNU convention and what `dist`
                // does with its own repeated flags.
                "--target" => {
                    let named = crate::args::value(&mut args, "--target")
                        .map_err(|e| format!("{e} (a target triple)"))?;
                    // An empty triple is refused here rather than in the shared
                    // rule, because it means different things to the two
                    // subcommands. `dist --target ""` already fails on its own -
                    // the value has to be one of three words. Here it would
                    // *silently succeed*: `release_dir` joins `Some("")` to
                    // `target/release`, the host directory, so `--target ""`
                    // measures whatever the last host build left there and
                    // reports a pass. `release_dir`'s own doc calls that "the one
                    // failure mode a size check must not have". Last-wins makes
                    // it worse rather than better: `--target aarch64-... --target
                    // ""` discards the triple the caller meant.
                    if named.is_empty() {
                        return Err("`--target` needs a target triple, not an empty \
                                    string (which would measure the host directory)"
                            .to_owned());
                    }
                    triple = Some(named);
                }
                other => return Err(format!("unknown argument `{other}`")),
            }
        }
        Ok(Invocation { triple })
    }
}

/// `cargo xtask size [--target <triple>]`.
///
/// # Errors
/// Returns a message when an argument is unrecognised, when a binary is missing
/// (with the `cargo build` line that would produce it), or when any binary is
/// over budget.
pub(crate) fn run(args: impl Iterator<Item = String>) -> Result<(), String> {
    let Invocation { triple } = Invocation::parse(args)?;

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

    /// Parse a command line written the way it would be typed, on `dist`'s own
    /// test helper's model.
    fn args(argv: &[&str]) -> Result<Invocation, String> {
        Invocation::parse(argv.iter().map(|s| (*s).to_owned()))
    }

    /// No `--target` means no triple, which is what
    /// [`naming_a_target_moves_where_the_binaries_are_looked_for`] then turns
    /// into `target/release`. Two steps, pinned separately, because the parse and
    /// the path join are different decisions.
    #[test]
    fn no_arguments_names_no_target() {
        assert_eq!(args(&[]), Ok(Invocation { triple: None }));
    }

    #[test]
    fn a_target_triple_is_carried_through() {
        assert_eq!(
            args(&["--target", "aarch64-apple-darwin"]),
            Ok(Invocation {
                triple: Some("aarch64-apple-darwin".to_owned())
            })
        );
    }

    /// The message names the argument, because the reader is a maintainer who
    /// has just typed something at a release.
    #[test]
    fn an_unknown_argument_is_rejected_by_name() {
        let err = args(&["--bogus"]).expect_err("`--bogus` is not an argument");
        assert!(err.contains("--bogus"), "{err}");
    }

    /// The message says what `--target` wants, not merely that it wants
    /// something.
    ///
    /// The shared rule's text is "`--target` needs a value", which is right for
    /// `dist`'s three flags and loses information here: this argument's value is
    /// a **build triple**, and the pre-hoist message said so. Asserting only
    /// `contains("--target")` would let that regress silently, which is how it
    /// regressed in the first place.
    #[test]
    fn a_target_with_no_value_is_told_what_it_wanted() {
        let err = args(&["--target"]).expect_err("nothing follows `--target`");
        assert!(err.contains("--target"), "{err}");
        assert!(err.contains("target triple"), "{err}");
    }

    /// An empty triple is refused rather than silently meaning "the host".
    ///
    /// `release_dir(root, Some(""))` joins to `target/release` - the host
    /// directory - so `size --target ""` would measure whatever the last host
    /// build left there and report a pass for a target it never looked at. That
    /// is the stale-binary failure `release_dir`'s own doc says a size check must
    /// not have. `dist --target ""` already fails, because its value must be one
    /// of three words; the two subcommands disagreed and now do not.
    #[test]
    fn an_empty_target_is_refused_rather_than_meaning_the_host() {
        let err = args(&["--target", ""]).expect_err("an empty triple is not a triple");
        assert!(err.contains("--target"), "{err}");

        // And last-wins must not launder it: the caller named a real triple and
        // a later empty one would discard it.
        let laundered = args(&["--target", "aarch64-apple-darwin", "--target", ""])
            .expect_err("the empty one is still empty");
        assert!(laundered.contains("--target"), "{laundered}");
    }

    /// A flag-shaped value is refused rather than used as a directory name.
    ///
    /// Red before [`crate::args::value`] was routed in here, and red at the
    /// site where it mattered: `size`'s own argument loop took the next token
    /// unconditionally, so `--target --release` measured
    /// `target/--release/release` and reported a missing binary at a path the
    /// user never typed - a message about the wrong problem entirely.
    /// `dist` had already decided this was wrong and guarded it; the two
    /// subcommands simply disagreed, and now share the rule.
    #[test]
    fn a_flag_shaped_target_value_is_refused_rather_than_used_as_a_directory() {
        let err = args(&["--target", "--release"]).expect_err("a flag is not a target triple");
        assert!(err.contains("--target"), "{err}");
        assert!(
            err.contains("--release"),
            "the message names what it found instead: {err}"
        );
    }

    /// A repeated flag takes the last value, matching `dist` and the GNU
    /// convention. Pinned rather than left to chance: it is the one argument
    /// rule here that is a *choice* rather than a rejection, so nothing else
    /// records it.
    #[test]
    fn a_repeated_target_takes_the_last_one() {
        assert_eq!(
            args(&["--target", "first", "--target", "second"]),
            Ok(Invocation {
                triple: Some("second".to_owned())
            })
        );
    }

    /// `run` refuses a bad command line before it touches the filesystem, so
    /// the rejection is not contingent on a release build existing.
    #[test]
    fn run_rejects_a_bad_command_line_without_measuring_anything() {
        let err = run(["--bogus"].into_iter().map(String::from))
            .expect_err("`--bogus` is not an argument");
        assert!(err.contains("--bogus"), "{err}");
        assert!(
            !err.contains("cargo build"),
            "it must not have got as far as looking for a binary: {err}"
        );
    }

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
            // ADR-0012's ledger recorded "17.21 MB" without a unit, which is the
            // ambiguity this module exists to end. Read here as decimal MB -
            // 17,210,000 rather than the 18,045,993 that 17.21 MiB would be -
            // because that is the *smaller* number and therefore the reading
            // most favourable to the binary. It fails the budget anyway, which
            // is the point: the P5 overage was not a rounding argument.
            bytes: 17_210_000,
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
