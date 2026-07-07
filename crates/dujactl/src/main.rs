//! `dujactl` — scriptable CLI for the running Duja app.
//!
//! v1 command surface (P5): `list`, `get`, `set <display|all> brightness
//! <n|±n>`, `input <display> <name|code>`, `doctor`, `version`.
//! Exit codes: 0 ok / 2 usage / 3 daemon-absent / 4 display-not-found /
//! 5 unsupported.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("version") | None => {
            let (core, ipc) = (duja_core::version(), duja_ipc::version());
            println!("dujactl {core} (ipc {ipc})");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("dujactl: unknown command `{other}` (commands arrive in P5; try `version`)");
            ExitCode::from(2)
        }
    }
}
