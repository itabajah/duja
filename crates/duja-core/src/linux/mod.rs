//! Pure Linux display rules, shared by the two Linux display backends.
//!
//! The counterpart of [`crate::macos`], and it exists for the same reason: two
//! backends need one rule and neither may depend on the other. `duja-ddc` owns
//! external monitors and reads the DRM connector tree to find them; `duja-panel`
//! owns the built-in panel and needs that same tree for the one thing a
//! backlight device cannot supply — an **identity**. `/sys/class/backlight`
//! carries a device name and a step count, no EDID and nothing durable, so a
//! Linux panel's [`StableDisplayId`](crate::id::StableDisplayId) can only come
//! from the internal connector's EDID. Two copies of that scan would be two
//! copies of a rule that must agree exactly.
//!
//! # This module reads files, and that is deliberate
//!
//! [`macos`](crate::macos) is arithmetic over values the caller already fetched;
//! this is not, because on Linux the "OS API" *is* a directory of text files.
//! Everything here goes through an **injected root** — `/` in production, a
//! `tempfile::TempDir` in tests — so there is no FFI, no platform gate, and no
//! behaviour a fixture cannot reproduce. That is what makes it belong in a crate
//! whose contract is testable-everywhere logic rather than in a backend: the
//! rules are exercised on all three CI lanes, on a project with no Linux
//! machine, which is the whole point.

pub mod drm;
