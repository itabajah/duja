# Duja fuzz targets

Coverage-guided fuzzers for `duja-core`'s total parsers, built with
[`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) and `libfuzzer-sys`.
There are four targets: `fuzz_caps_string` (the MCCS capability-string parser),
`fuzz_edid_parse` (`EdidInfo::parse` + `StableDisplayId::from_edid`),
`fuzz_quirks_toml` (the quirk-database parser), and `fuzz_ipc_frame` (the
`duja-ipc` length-prefixed frame decoders `read_request` / `read_response` /
`read_frame_bytes`). Each simply feeds the raw input
bytes to its parser and relies on libFuzzer to flag any panic, hang, or
out-of-memory — the parsers are contractually total, so a crash is a bug. This
crate is a **separate Cargo workspace** (see the `[workspace]` table in
`Cargo.toml`) so the `libfuzzer-sys` dependency never enters the main build
graph or release lockfile. It compiles under stable
(`cargo check --manifest-path fuzz/Cargo.toml`), which CI uses to keep the
targets from bit-rotting, but **running** a fuzzer needs a nightly toolchain for
SanitizerCoverage instrumentation.

To run: install the tools once with `rustup toolchain install nightly` and
`cargo install cargo-fuzz`, then from the repo root run e.g.
`cargo +nightly fuzz run fuzz_caps_string` (add `-- -max_total_time=300` for a
timed session; `cargo +nightly fuzz list` shows all targets).

**Windows note:** the default (address-sanitizer) build links
`clang_rt.asan_dynamic-x86_64.dll`, which is not on `PATH` by default. Before
running, prepend the MSVC host bin directory, e.g.
`$env:Path = "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\<ver>\bin\Hostx64\x64;$env:Path"`
(otherwise the target exits with `0xc0000135`, a missing-DLL error). Do not
retry with `-s none` after an ASan run without `cargo clean` — mixing sanitizer
modes produces an `unresolved external symbol __start___sancov_cntrs` link error.

Last full burn (2026-07-08): 1,000,000 executions per target, zero crashes
(`fuzz_caps_string` 52k exec/s, `fuzz_edid_parse` 200k exec/s,
`fuzz_quirks_toml` 4.4k exec/s). Committed seeds
live in `fuzz/corpus/<target>/` — the real MSI MP273QP capability string, a
valid synthetic 128-byte EDID, and the embedded `quirks.toml`. **Corpus
policy:** keep the seeds small and meaningful (one valid, exercising sample per
target is enough to bootstrap coverage); do not commit machine-generated corpus
growth or `fuzz/artifacts/`. Any crash-reproducing input libFuzzer minimizes
should be turned into a unit test in `duja-core` rather than left in the corpus.
