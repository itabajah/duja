# Duja fuzz targets

Coverage-guided fuzzers for Duja's total parsers, built with
[`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) and `libfuzzer-sys`.
There are six targets: `fuzz_caps_string` (the MCCS capability-string parser),
`fuzz_edid_parse` (`EdidInfo::parse` + `StableDisplayId::from_edid`),
`fuzz_quirks_toml` (the quirk-database parser), `fuzz_ipc_frame` (the
`duja-ipc` length-prefixed frame decoders `read_request` / `read_response` /
`read_frame_bytes`), `fuzz_ddc_packet` (the `duja-ddc` DDC/CI reply decoders
`decode_get_vcp_reply` / `decode_caps_reply`), and `fuzz_config_toml` (the
user-editable `config.toml`: parse, then the migration chain, then the typed
deserialize). Five of the six feed raw bytes to one parser and rely on libFuzzer
to flag any panic, hang, or out-of-memory: the parsers are contractually total,
so a crash is a bug. `fuzz_config_toml` drives three stages instead of one,
because a config file fails in three different places and only the first is a
parser - its module header says which and why.

**The target list is checked rather than remembered.** An xtask test reads
`Cargo.toml`'s `[[bin]]` entries and `.github/workflows/fuzz.yml`'s matrix and
fails if they disagree. A target declared here and missing from the matrix is a
fuzzer that compiles, appears in `cargo fuzz list`, and is never run by
anything - it looks exactly like coverage and is none. This
crate is a **separate Cargo workspace** (see the `[workspace]` table in
`Cargo.toml`) so the `libfuzzer-sys` dependency never enters the main build
graph or release lockfile. It compiles under stable
(`cargo check --manifest-path fuzz/Cargo.toml --all-targets`), which is a step in
CI's `clippy (ubuntu-latest)` job - inside an already-required check rather than
in a job of its own, so it is enforced from the first PR - but **running** a
fuzzer needs a nightly toolchain for SanitizerCoverage instrumentation.
`.github/workflows/fuzz.yml` does that weekly, on Sundays, and uploads the
crashing input if a target finds one.

To run: install the tools once with `rustup toolchain install nightly` and
`cargo install cargo-fuzz`, then from the repo root run e.g.
`cargo +nightly fuzz run fuzz_caps_string` (add `-- -max_total_time=300` for a
timed session; `cargo +nightly fuzz list` shows all targets).

**Windows note:** the default (address-sanitizer) build links
`clang_rt.asan_dynamic-x86_64.dll`, which is not on `PATH` by default. Before
running, prepend the MSVC host bin directory, e.g.
`$env:Path = "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\<ver>\bin\Hostx64\x64;$env:Path"`
(otherwise the target exits with `0xc0000135`, a missing-DLL error). Do not
retry with `-s none` after an ASan run without `cargo clean`; mixing sanitizer
modes produces an `unresolved external symbol __start___sancov_cntrs` link error.

Last full **manual** burn (2026-07-08): 1,000,000 executions per target, zero
crashes (`fuzz_caps_string` 52k exec/s, `fuzz_edid_parse` 200k exec/s,
`fuzz_quirks_toml` 4.4k exec/s). That predates `fuzz_config_toml`, which has
never been burned; the first scheduled run is what gives it a number. Committed seeds
live in `fuzz/corpus/<target>/`: the real MSI MP273QP capability string, a
valid synthetic 128-byte EDID, and the embedded `quirks.toml`. **Corpus
policy:** keep the seeds small and meaningful (one valid, exercising sample per
target is enough to bootstrap coverage); do not commit machine-generated corpus
growth or `fuzz/artifacts/`. Any crash-reproducing input libFuzzer minimizes
should be turned into a unit test in `duja-core` rather than left in the corpus.
