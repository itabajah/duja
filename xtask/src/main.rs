//! `cargo xtask` — workspace automation.
//!
//! **The task list lives in [`HELP`], and only there.** Tasks land alongside the
//! phases that need them; what exists today is what `cargo xtask help` prints.
//!
//! This paragraph used to enumerate them, and got the enumeration wrong three
//! times in a row. `size` was missing from the day it shipped (P8 wave 1).
//! `dist`'s entry named two of its three targets from the day the third landed
//! (P7 wave 6). And the correction for those two left `licenses` and `tr-extract`
//! listed with `(P5)` / `(P4)` markers in the same shape used for tasks that
//! exist, when **neither has ever been written** - no module, no match arm - and
//! `HELP`'s own last line says so.
//!
//! `HELP` was right every time, which is the whole lesson: it is what a user
//! reads when the tool disappoints them, so it gets fixed; a module doc that
//! enumerates is read by nobody until it is wrong, and then it reads as a
//! complete list. Three corrections is the point at which this project's own
//! rule applies - remove the thing rather than correct it again - so the list is
//! gone and `every_task_in_help_has_a_module` (a `cfg(test)` item, so
//! unlinked here) pins what replaced it.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod args;
mod bundle;
mod dist;
mod macho;
mod size;
mod version;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const HELP: &str = "\
xtask — Duja workspace automation

USAGE: cargo xtask <task>

TASKS:
  help                       show this help
  dist --version X.Y.Z       stage the shippable artifact for this host
                             (Windows: portable zip; macOS: Duja.app + DMG;
                              Linux: portable tarball)
       [--target windows|macos|linux]  package for a platform other than the host
       [--sign <identity>]       codesign identity for the macOS bundle
                                 (default `-`, an ad-hoc signature)
  size                       measure the release binaries against their byte
                             budgets (ADR-0012); exits non-zero if either is over
       [--target <triple>]       look under target/<triple>/release instead
  (licenses, tr-extract arrive in later phases)
";

fn main() -> ExitCode {
    let mut args = std::env::args();
    let _bin = args.next(); // argv[0]
    match args.next().as_deref() {
        Some("help") | None => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("dist") => match dist::run(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("xtask dist: {msg}");
                ExitCode::from(1)
            }
        },
        Some("size") => match size::run(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("xtask size: {msg}");
                ExitCode::from(1)
            }
        },
        Some(other) => {
            eprintln!("xtask: unknown task `{other}`\n\n{HELP}");
            ExitCode::from(2)
        }
    }
}

/// The repository root — the parent of this crate's manifest directory.
///
/// # Errors
/// Returns a message if the manifest directory has no parent, which would mean
/// the crate has been moved out of the workspace.
fn repo_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "cannot resolve the repository root".to_owned())
}

/// Read a repository file by path components, for the tests that pin a constant
/// here against the *other* file that has to agree with it.
///
/// Several couplings in this crate cross a boundary no type can span — a
/// constant in another crate, a value in a YAML workflow, a `[[bin]]` name in a
/// manifest. Restating them in a comment is what rots; reading the other file is
/// what does not. Panics with the path if it cannot be read, which is the
/// intended behaviour: a moved file must fail loudly, not silently stop checking.
#[cfg(test)]
fn read_repo_file(parts: &[&str]) -> String {
    let mut path = repo_root().expect("repo root");
    for part in parts {
        path.push(part);
    }
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    /// Every task [`HELP`](crate::HELP) advertises is one `main` can dispatch,
    /// and every task `main` dispatches is one `HELP` advertises.
    ///
    /// Written because the *prose* version of this coupling drifted three times
    /// (the module doc omitted `size`, then named two of `dist`'s three targets,
    /// then listed two tasks that have never existed) and each drift was found
    /// by a human reading the diff rather than by anything that could fail.
    /// `HELP` was correct throughout, so what was missing is not a better
    /// comment but a comparison.
    ///
    /// Both directions matter and they fail differently. A task in `HELP` that
    /// `main` cannot dispatch is a documented command that answers `unknown
    /// task`; a task `main` dispatches that `HELP` omits is a feature nobody can
    /// find. The second is what shipped for four months.
    ///
    /// `HELP`'s "arrive in later phases" line is deliberately *excluded* by the
    /// two-space-indent rule below, and that exclusion is the interesting part:
    /// it is how `HELP` stays able to name work that does not exist yet without
    /// this test demanding a module for it.
    #[test]
    fn every_task_in_help_has_a_module() {
        // A task line in HELP is indented exactly two spaces and starts with the
        // task word. Deeper indents are that task's options, and the trailing
        // parenthetical about later phases starts with `(`.
        let advertised: Vec<&str> = crate::HELP
            .lines()
            .filter_map(|line| line.strip_prefix("  "))
            .filter(|rest| !rest.starts_with(' ') && !rest.starts_with('('))
            .filter_map(|rest| rest.split_whitespace().next())
            .filter(|task| *task != "help")
            .collect();

        // What `main` actually routes. Restated here rather than parsed out of
        // the match, because a test that derived both sides from the same source
        // would compare a thing with itself.
        let dispatched = ["dist", "size"];

        assert_eq!(
            advertised, dispatched,
            "HELP advertises {advertised:?} and main dispatches {dispatched:?}. A \
             task in HELP that main cannot run answers `unknown task`; one main \
             runs that HELP omits is a feature nobody can find."
        );

        // The tripwire. Both sides are short lists, so a parse that silently
        // matched nothing would make the comparison above vacuously true against
        // an equally empty `dispatched` only if someone emptied that too - but a
        // changed HELP layout could empty just the left side, and then this fires
        // instead of the assertion above passing for the wrong reason.
        assert!(
            advertised.len() >= 2,
            "only {} task lines parsed out of HELP - the parse is broken, not the \
             list",
            advertised.len()
        );

        // And the module doc must not have grown its own list back. The three
        // drifts were all in that block; the rule now is that it names no tasks.
        let source = crate::read_repo_file(&["xtask", "src", "main.rs"]);
        let header: String = source
            .lines()
            .take_while(|line| line.starts_with("//!"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            header.contains("only there"),
            "the module header no longer says the task list lives in HELP alone; \
             if it has grown an enumeration back, that is the drift this test \
             exists for"
        );
    }

    /// The number of `|`-delimited fields a GitHub-flavoured Markdown row splits
    /// into, with a leading and a trailing pipe discounted.
    ///
    /// Two rules from the [GFM tables extension], both load-bearing:
    ///
    /// - **A `|` preceded by a backslash is content, not a separator, *including
    ///   inside other inline spans*.** The spec makes the narrower claim — "include
    ///   a pipe in a cell's content by escaping it, including inside other inline
    ///   spans", with `` b `\|` az `` as its example — and the broader lookbehind
    ///   comes from cmark-gfm's scanner rather than from the spec; see below. What
    ///   both agree on is the half that matters here: an *unescaped* `|` inside
    ///   backticks really does split the cell, which is how a
    ///   `` `O_NOFOLLOW | O_DIRECTORY` `` in `debt.md` silently shunted a row's
    ///   cells one column left and dropped its last one.
    /// - **A leading and trailing pipe is optional.** Discounting one of each
    ///   means both styles count the same, so a row written without a trailing
    ///   pipe cannot raise a false alarm here. (A row written without a *leading*
    ///   one never reaches this function — see the test's own docs.)
    ///
    /// "Preceded by a backslash" is the whole escape rule, and it is deliberately
    /// **not** a `CommonMark` escape state machine. cmark-gfm scans a cell with
    /// `table_cell = (escaped_char|[^|\r\n])+` where
    /// `escaped_char = [\\][|!"#…\\\]…]` — the escapable set contains the
    /// backslash — and re2c takes the longest match. So in `a\\|b` the longest
    /// parse is `a`, `\`, `\|`, `b`: the pipe is **absorbed**, not a delimiter.
    /// An escape-state machine says the opposite (the first `\` escapes the
    /// second, leaving the pipe bare), and two versions of this function said it
    /// before the scanner source was read. Whenever a `|` follows a backslash,
    /// some longest parse absorbs it, so the one-byte lookbehind *is* the rule.
    ///
    /// One shape is still knowingly not GFM's: a line that is a bare `|` answers
    /// 1 here, where cmark-gfm's row parser consumes the leading pipe and ends
    /// with no columns at all. Unreachable — no line under `docs/` is a bare
    /// pipe, and such a line is not a table row in any case.
    ///
    /// [GFM tables extension]: https://github.github.com/gfm/#tables-extension-
    fn cell_count(line: &str) -> usize {
        let mut fields: usize = 1;
        let mut previous = 0_u8;
        let mut ends_on_separator = false;
        for byte in line.bytes() {
            ends_on_separator = byte == b'|' && previous != b'\\';
            if ends_on_separator {
                fields = fields.saturating_add(1);
            }
            previous = byte;
        }
        // A leading pipe contributes an empty first field and a trailing one an
        // empty last, and both discounts read the same `ends_on_separator` the
        // loop set — an earlier version tested the tail with a separate
        // `ends_with` suffix check, and a checker whose two halves can disagree is
        // worth less than the bug it looks for. `saturating_sub` rather than `-`
        // so a pipe-less line (which the caller never passes, but a future one
        // might) cannot underflow, and the length guard stops a lone `"|"` being
        // discounted twice.
        let lead = usize::from(line.starts_with('|'));
        let trail = usize::from(ends_on_separator && line.len() > 1);
        fields.saturating_sub(lead).saturating_sub(trail)
    }

    /// [`cell_count`] against the shapes its own docs argue about.
    ///
    /// Until this existed, the only exercise the function got was the corpus walk
    /// below — and the backslash rule its documentation spends a paragraph on was
    /// covered by exactly one line of `docs/debt.md`. Editing that row would have
    /// silently removed the coverage for a rule nothing else touches.
    #[test]
    fn cell_count_agrees_with_cmark_gfm_on_the_shapes_it_documents() {
        // The ordinary rows, with and without the optional delimiters. GFM makes
        // both leading and trailing pipes optional, so all four must agree.
        assert_eq!(cell_count("|a|b|"), 2);
        assert_eq!(cell_count("a|b"), 2);
        assert_eq!(cell_count("|a|b"), 2);
        assert_eq!(cell_count("a|b|"), 2);
        assert_eq!(cell_count("|---|---|"), 2, "the delimiter row is a row");

        // The escape rule, which is the whole reason this is not a `split('|')`.
        // An escaped pipe is content; the row is one cell.
        assert_eq!(cell_count("|a \\| b|"), 1);
        // And the shape a CommonMark escape machine gets wrong: cmark-gfm's
        // longest match parses `a`, `\`, `\|`, `b`, absorbing the pipe. Two
        // rewrites of this function disagreed with that before the scanner source
        // was read.
        assert_eq!(cell_count("|a \\\\| b|"), 1);
        assert_eq!(cell_count("|a \\\\\\| b|"), 1);
        // A row ending in an escaped pipe has no trailing delimiter to discount.
        assert_eq!(cell_count("|a\\|"), 1);

        // The degenerate shapes the discount arithmetic has to survive.
        assert_eq!(cell_count("||"), 1, "one empty cell between two delimiters");
        assert_eq!(
            cell_count("|"),
            1,
            "the documented divergence from cmark-gfm"
        );
        assert_eq!(cell_count("abc"), 1, "no pipes at all");
        assert_eq!(cell_count(""), 1, "and the empty line cannot underflow");

        // A pipe inside a code span still splits, which is how an unescaped one in
        // `debt.md` ate a row's last cell for months.
        assert_eq!(cell_count("| `a | b` | c |"), 3);
    }

    /// A row whose cell count disagrees with its header's.
    #[derive(Debug, PartialEq, Eq)]
    struct Mismatch {
        /// 1-based line number of the offending row.
        line: usize,
        /// How many cells the row has.
        cells: usize,
        /// How many its header has.
        width: usize,
        /// 1-based line number of that header.
        header_line: usize,
    }

    /// Every row in `text` whose cell count disagrees with its header's, and how
    /// many rows were compared to find them.
    ///
    /// Split out of [`every_docs_table_row_matches_its_header`] rather than
    /// written inline, because inline it could only ever be exercised by whatever
    /// the real corpus happens to contain: with no file under `docs/` drawing a
    /// table inside a code fence, and none carrying a mismatched row, both the
    /// fence toggle and the comparison itself could be deleted with the suite
    /// green. The walk asserts; this decides.
    fn table_mismatches(text: &str) -> (Vec<Mismatch>, usize) {
        let mut found = Vec::new();
        let mut rows = 0_usize;
        let mut header: Option<(usize, usize)> = None;
        let mut fenced = false;
        for (number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.starts_with("```") {
                fenced = !fenced;
                // A fence line is not a row, and it also ends any table above it.
                header = None;
                continue;
            }
            if fenced {
                continue;
            }
            if !line.starts_with('|') {
                // Any non-row line ends the table; the next one that starts
                // with a pipe is a fresh header.
                header = None;
                continue;
            }
            let cells = cell_count(line);
            match header {
                Some((width, at)) => {
                    rows = rows.saturating_add(1);
                    if cells != width {
                        found.push(Mismatch {
                            line: number.saturating_add(1),
                            cells,
                            width,
                            header_line: at,
                        });
                    }
                }
                None => header = Some((cells, number.saturating_add(1))),
            }
        }
        (found, rows)
    }

    /// [`table_mismatches`] against the shapes the corpus does not contain.
    #[test]
    fn table_mismatches_reports_the_rows_that_disagree_with_their_header() {
        // Nothing under `docs/` has a mismatched row - that is the point of the
        // check - so without a fixture the comparison could be replaced by
        // `assert_eq!(cells, cells)` and stay green forever.
        let (found, rows) = table_mismatches("| a | b |\n| --- | --- |\n| c |\n");
        assert_eq!(
            found,
            vec![Mismatch {
                line: 3,
                cells: 1,
                width: 2,
                header_line: 1
            }]
        );
        assert_eq!(rows, 2, "the delimiter row is compared like any other");

        // An *extra* cell is the direction the walk exists for: it renders as a
        // correct table and leaves text nobody reads.
        let (extra, _) = table_mismatches("| a | b |\n| --- | --- |\n| c | d | e |\n");
        assert_eq!(extra.len(), 1);
        assert_eq!(extra.first().map(|m| m.cells), Some(3));

        // A well-formed table reports nothing, and counts what it compared.
        let (none, counted) = table_mismatches("| a | b |\n| --- | --- |\n| c | d |\n| e | f |\n");
        assert!(none.is_empty());
        assert_eq!(counted, 3);

        // A blank line ends the table, so the second one is measured against its
        // own header rather than the first one's.
        let (two_tables, _) =
            table_mismatches("| a | b |\n| --- | --- |\n\n| c |\n| --- |\n| d |\n");
        assert!(two_tables.is_empty(), "{two_tables:?}");
    }

    /// The fence rule, which the corpus cannot exercise.
    #[test]
    fn a_table_drawn_inside_a_code_fence_is_illustration_rather_than_data() {
        // No file under `docs/` draws a table inside a fence today, so deleting
        // the toggle changes nothing about the real corpus and everything about
        // the first document that adds one - a shape this project's own docs
        // invite, since they explain the table rules by showing rows.
        let (found, rows) = table_mismatches("```text\n| a | b |\n| --- |\n```\n");
        assert!(found.is_empty(), "{found:?}");
        assert_eq!(rows, 0, "nothing inside a fence is compared");

        // And the fence has to close: a table after it is checked again.
        let (after, _) =
            table_mismatches("```text\n| a | b |\n```\n\n| c | d |\n| --- | --- |\n| e |\n");
        assert_eq!(after.len(), 1, "{after:?}");
        assert_eq!(after.first().map(|m| m.line), Some(7));

        // A fence ends the table above it, so a row *after* the fence is a fresh
        // header rather than a mismatch against the table before it. The first
        // version of this case put nothing after the closing fence, which made it
        // pass whether or not the fence ended anything — the fenced lines were
        // skipped either way, so it compared nothing and asserted that nothing was
        // wrong. The row below is what gives the assertion something to be wrong
        // about.
        let (straddling, rows_across) =
            table_mismatches("| a | b |\n```text\nx\n```\n| c |\n| --- |\n| d |\n");
        assert!(straddling.is_empty(), "{straddling:?}");
        assert_eq!(rows_across, 2, "both rows of the second table, and no more");
    }

    /// The fuzz targets named in `fuzz/Cargo.toml` and the ones `fuzz.yml`
    /// actually burns must be the same set.
    ///
    /// Three files have to agree about this list and none of them can see the
    /// others: the manifest declares the `[[bin]]`s, the workflow enumerates a
    /// matrix, and `fuzz/README.md` tells a human how many there are - all three
    /// are checked here. The interesting direction is **manifest-only**: a target added to the
    /// manifest and forgotten in the matrix compiles, passes the `cargo check`
    /// step in CI, appears in `cargo fuzz list`, and is never run by anything.
    /// It looks exactly like coverage and is none.
    ///
    /// The other direction fails loudly on its own (`cargo fuzz run` errors on
    /// an unknown target) but only once a week, so it is checked here too.
    ///
    /// `fuzz/` is a separate workspace, so no `cargo` command in the main build
    /// graph can be made to notice any of this.
    #[test]
    fn every_declared_fuzz_target_is_in_the_weekly_burn() {
        let manifest = crate::read_repo_file(&["fuzz", "Cargo.toml"]);
        let workflow = crate::read_repo_file(&[".github", "workflows", "fuzz.yml"]);

        // `name = "fuzz_x"` under a `[[bin]]`. Matching on the prefix rather
        // than on section state keeps this to one pass; the package's own
        // `name = "duja-fuzz"` does not start with `fuzz_`, and neither does any
        // dependency key.
        let mut declared: Vec<String> = manifest
            .lines()
            .filter_map(|line| line.trim().strip_prefix("name = \""))
            .filter_map(|rest| rest.strip_suffix('"'))
            .filter(|name| name.starts_with("fuzz_"))
            .map(str::to_owned)
            .collect();

        // A YAML sequence item under the `target:` key. `fuzz_targets` (the
        // directory) never appears in this shape, and `${{ matrix.target }}` is
        // not a literal, so neither can be mistaken for an entry.
        let mut burned: Vec<String> = workflow
            .lines()
            .filter_map(|line| line.trim().strip_prefix("- "))
            .filter(|name| name.starts_with("fuzz_"))
            .map(str::to_owned)
            .collect();

        declared.sort();
        burned.sort();
        assert_eq!(
            declared, burned,
            "fuzz/Cargo.toml declares {declared:?} and .github/workflows/fuzz.yml \
             burns {burned:?}. A target in the manifest and not in the matrix is \
             a fuzzer that exists, compiles, lists, and is never run."
        );

        // The tripwire. Both sides are parsed by prefix matching, so a change to
        // either file's shape could leave both lists empty and the comparison
        // above vacuously true. Six is the count as of P8 wave 2; the floor is
        // deliberately below it rather than equal to it, because a *removed*
        // target is a decision somebody makes on purpose and should not have to
        // edit an assertion to express.
        assert!(
            declared.len() >= 5,
            "only {} fuzz targets parsed out of fuzz/Cargo.toml - the parse is \
             broken, not the list",
            declared.len()
        );

        // The third file. `fuzz/README.md` opens by telling a human how many
        // targets there are, and a reader who counts on that sentence is the
        // one this catches - the number is prose, so nothing else can.
        let readme = crate::read_repo_file(&["fuzz", "README.md"]);
        let spelled = [
            "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
            "eleven", "twelve",
        ]
        .get(declared.len())
        .copied()
        .unwrap_or("many");
        let sentence = format!("There are {spelled} targets");
        assert!(
            readme.contains(&sentence),
            "fuzz/README.md does not say `{sentence}`, and there are {} targets. \
             The count is prose: nothing but this test reads it.",
            declared.len()
        );
    }

    /// Every Markdown file under `docs/`, recursively.
    fn markdown_files(dir: &PathBuf) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let entries =
            std::fs::read_dir(dir).unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                found.extend(markdown_files(&path));
            } else if path.extension().is_some_and(|ext| ext == "md") {
                found.push(path);
            }
        }
        found
    }

    /// Every row of every Markdown table under `docs/` **that this check can
    /// see** must have exactly as many cells as its own header.
    ///
    /// The qualification is not throat-clearing: a table whose rows omit the
    /// leading pipe is skipped rather than checked, so "every" would be an
    /// over-claim in a coverage sense rather than a false-alarm one. The
    /// paragraph below lists that with the four constructs that would produce a
    /// *wrong* answer rather than no answer.
    ///
    /// GFM does not require this — "if there are a number of cells fewer than the
    /// number of cells in the header row, empty cells are inserted; if there are
    /// greater, the excess is ignored" — and that permissiveness is the whole
    /// problem. An **extra** cell renders as a correct table and leaves text in
    /// the file that nobody reads, which is the worst possible combination for
    /// `debt.md`, a document consulted by grepping far more often than by
    /// rendering. It is how a stale, already-retracted "Why deferred" cell
    /// survived a correction to the row above it in `#132`, and how another row's
    /// last cell went invisible the day `#114` added an unescaped `|` to it. (Not
    /// "since v0.1.1", which an earlier version of this sentence said by reading
    /// the row's *When* column instead of dating the break.)
    ///
    /// The delimiter row is checked like any other, which is stricter than
    /// necessary in one direction and exactly right in the other: GFM says a
    /// header and delimiter that disagree mean **no table is recognised at all**,
    /// so that mismatch turns a table into a paragraph of pipes.
    ///
    /// Fenced code blocks are skipped: a table drawn inside one is illustration,
    /// not data. That rule, and the comparison itself, live in
    /// [`table_mismatches`] so that both can be driven by fixtures — no file under
    /// `docs/` contains either a fenced table or a mismatched row, so inline they
    /// were code no test could reach.
    ///
    /// Four things it deliberately does not model, none of which any file under
    /// `docs/` uses today: `~~~` fences (only backticks toggle), indented code
    /// blocks, raw HTML blocks (`docs/qa-checklist.md` has multi-line `<!-- -->`
    /// comments, though none with a pipe in it), and the requirement that a
    /// delimiter row actually follow the presumed header — so a `|`-wrapped prose
    /// line would be taken for a header and could fail the real table beneath it.
    /// Each is a line of code and a class of false alarm; they are named here so
    /// that a future false alarm is diagnosable rather than mysterious.
    ///
    /// A row with **no leading pipe** is a fifth, and a different shape: those are
    /// not scanned at all rather than scanned wrongly, so such a table is silently
    /// unchecked. GFM allows it; nothing under `docs/` writes it.
    #[test]
    fn every_docs_table_row_matches_its_header() {
        let mut docs = crate::repo_root().expect("repo root");
        docs.push("docs");
        let files = markdown_files(&docs);
        let mut rows_checked = 0_usize;

        for path in &files {
            let text = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            let name = path.strip_prefix(&docs).unwrap_or(path).display();
            let (found, rows) = table_mismatches(&text);
            rows_checked = rows_checked.saturating_add(rows);
            if let Some(bad) = found.first() {
                panic!(
                    "docs/{name} line {} has {} cells; its header at line {} has {}. An extra \
                     cell renders correctly and is read by nobody — see this test's docs.",
                    bad.line, bad.cells, bad.header_line, bad.width
                );
            }
        }

        // The guards that stop this check quietly covering nothing. The first
        // version of it named four files, two of which have no tables at all, and
        // missed six that do — including the ADR this project tells new backends
        // to read. A walk plus a floor cannot rot the same way.
        //
        // Both floors are tripwires rather than rules, and neither can be pinned
        // by a test: lowering `100` to `0` leaves every test in this file green,
        // because the only thing that would notice is this assertion. The `panic!`
        // above is the same shape — the corpus has no mismatched row, so deleting
        // the reaction survives everything. Both are written down so the next
        // mutation census does not read the survivors as evidence that either is
        // pointless; what a fixture *can* pin is the decision, and
        // [`table_mismatches`] now takes it.
        //
        // The numbers are measured rather than guessed, because the first version
        // of this comment guessed: it put the row floor at 200 and called that
        // "roughly half of what the corpus carries", when the corpus then carried
        // 258 rows across 31 files and 200 is 78 % of them. A floor is only a
        // tripwire if the thing it trips on is a broken walk, and a broken walk
        // reports approximately zero — with one exception, which is why there are
        // two floors rather than one. Re-measured against the corpus as it stands
        // after the 2026-08-07 docs checkpoint (34 files, 307 rows, of which
        // `docs/debt.md` holds 103 and `docs/debt-archive.md` 19):
        //
        // - **the walk stops descending.** Delete the recursion from
        //   `markdown_files` and it finds the 10 top-level files but still 257
        //   rows, 84 % of the corpus, clearing any row floor worth setting. Only
        //   the file floor catches this.
        // - **the scan stops recognising rows.** Invert the fence state and all 34
        //   files are still walked while 0 rows are compared. Only the row floor
        //   catches this.
        //
        // One pressure on the row floor was removed by that checkpoint rather than
        // by this test. Draining a debt row used to delete it, so `debt.md`'s own
        // preamble promised as routine an edit that could halve the largest single
        // contributor to the corpus; draining now *moves* the row to
        // `debt-archive.md`, and the total is unchanged. The floor no longer has to
        // survive ordinary pruning of this file, only a broken walk.
        //
        // Two figures have been struck from this comment for not reproducing, so
        // both above were measured against the live corpus rather than reasoned
        // about. The first was "delete the `push` and it finds 24 files and 47
        // rows": deleting that arm pushes nothing at any depth, so it finds 0 and 0
        // and both floors fire. The second was this bullet's own "or break the
        // leading-pipe test" — dropping the `!` from `if !line.starts_with('|')`
        // inverts which lines are rows and compares roughly 4,800 of them, clearing
        // the row floor by nearly fifty times. It is caught, but by the mismatch
        // `panic!` rather than by either floor, which is a different claim than the
        // one this bullet is making.
        //
        // 100 rows and 10 files sit well under any plausible pruning and well over
        // both failure modes.
        assert!(
            files.len() >= 10,
            "only {} markdown files under docs/ — the walk is not walking",
            files.len()
        );
        assert!(
            rows_checked >= 100,
            "only {rows_checked} table rows checked — the tables are no longer being found"
        );
    }
}
