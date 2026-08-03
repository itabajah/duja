//! Pure scanning of the Linux DRM connector tree in sysfs.
//!
//! Every Linux display Duja can control is a **DRM connector**: a directory
//! under `/sys/class/drm` named `card<N>-<TYPE>-<INDEX>` (`card0-DP-1`,
//! `card0-eDP-1`, `card1-HDMI-A-2`). Each carries the three things enumeration
//! needs — whether something is plugged in (`status`), the display's raw EDID
//! (`edid`), and, on a driver that exposes it, the I2C adapter the monitor's
//! DDC/CI channel lives on (`ddc`).
//!
//! This module reads all of that through an **injected root**: `/` in
//! production, a `tempfile::TempDir` in tests. That is why it needs no platform
//! gate at all — the tree is plain files and directories, so a fixture
//! reproduces it exactly on any host, and every rule below is compiled and
//! exercised on all three CI lanes. (The `cfg(any(test, target_os = "linux"))`
//! gate its *callers* carry is for their own Linux-only halves; this module is
//! public API that two crates consume, so it is unconditional.) What is left for
//! `duja-ddc`'s Linux `sys` module is opening `/dev/i2c-<N>` and the ioctl,
//! which is genuinely unreproducible.
//!
//! # Why `ddc/i2c-dev/i2c-<N>` and not the `ddc` symlink's own target
//!
//! `<connector>/ddc` is a symlink to the I2C adapter the kernel bound to this
//! connector, so `readlink` alone would name the adapter — but naming it is not
//! the question. The question is whether `/dev/i2c-<N>` **exists**, and it does
//! not until the `i2c-dev` module is loaded. That module is exactly what
//! publishes the `i2c-dev/i2c-<N>` child, so keying on the child answers the
//! question that matters instead of the one that is easy. Its absence is
//! reported as [`NoI2c::NoI2cDev`] rather than folded into a plain "no DDC
//! here", because the two have different remedies and `dujactl doctor` has to
//! say which one applies.
//!
//! Reading through the symlink also means the fixtures need no symlinks: an
//! ordinary directory at `<connector>/ddc` is indistinguishable to `read_dir`,
//! and creating a symlink on Windows needs a privilege CI does not grant.

use std::fs;
use std::io;
use std::path::Path;

/// One EDID block. A connector reporting fewer bytes than this has no identity
/// in it.
const EDID_BLOCK_LEN: usize = 128;

/// The connector types that name a **built-in** panel.
///
/// From the kernel's `drm_connector_enum_list` (`drm_connector.c`), which is
/// also what produces the sysfs directory names. Compared case-insensitively
/// because the kernel's spelling is mixed (`eDP`, `LVDS`, `DSI`).
const INTERNAL_TYPES: [&str; 4] = ["edp", "lvds", "dsi", "dpi"];

/// Why a connected DRM connector has no DDC/CI bus Duja can open.
///
/// Kept apart from "no connector" because the remedies differ, and
/// [`NoI2cDev`](Self::NoI2cDev) in particular is a *fixable* state a user should
/// be told about rather than a property of their hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoI2c {
    /// The connector exposes no `ddc` entry at all: the graphics driver does not
    /// publish this connector's I2C adapter, so there is nothing to open. Not
    /// user-fixable.
    NoDdcLink,
    /// A `ddc` entry exists, but has no `i2c-dev/i2c-<N>` child — the `i2c-dev`
    /// kernel module is not loaded, so no `/dev/i2c-*` character device exists
    /// for any adapter. User-fixable (`modprobe i2c-dev`).
    NoI2cDev,
}

/// One connected DRM connector, as plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrmConnector {
    /// The connector's name **without** the `card<N>-` prefix — `DP-1`,
    /// `eDP-1`, `HDMI-A-2`.
    ///
    /// The prefix is dropped because this is the form the display server uses,
    /// which makes the name the join key between an identity discovered here and
    /// the desktop rectangle discovered from X11 or Wayland. Sysfs is the only
    /// place the `card<N>-` prefix exists at all.
    ///
    /// **That join is not universal, and the joiner does not assume it is.** It
    /// holds for the modesetting DDX and for DRM-backed Wayland compositors,
    /// which is the modern stack. It is reported not to hold for the NVIDIA
    /// proprietary X11 driver, which names outputs on its own indexing (`DP-0`,
    /// `HDMI-0`), nor for the legacy `xf86-video-intel` DDX, which omits the
    /// hyphen before the index (`eDP1`, `DP1`) and so defeats a string-equality
    /// join outright. Neither is verified by this project.
    ///
    /// So the name is carried as the *best* join key available, not as a
    /// guarantee, and `duja_dimmer::linux_outputs` supplies the fallback: the
    /// [`edid`](Self::edid) below. That module also declines to trust a name
    /// match that a present EDID contradicts — the NVIDIA namespaces overlap and
    /// are offset by one, so a name equality there is a *wrong* answer rather
    /// than a missing one.
    pub name: String,
    /// The raw EDID bytes: at least one 128-byte block.
    pub edid: Vec<u8>,
    /// The I2C adapter index behind this connector's DDC/CI channel, or why
    /// there is none.
    pub i2c: Result<u32, NoI2c>,
    /// Whether this connector drives a built-in panel (eDP, LVDS, DSI, DPI)
    /// rather than an external monitor.
    ///
    /// A built-in panel belongs to `duja-panel` and its backlight, not to this
    /// backend — DDC/CI does not reach one. It is reported rather than dropped
    /// so the caller can classify it, exactly as the Windows backend surfaces
    /// `is_internal` instead of filtering.
    pub is_internal: bool,
}

/// Scan `<root>/sys/class/drm` and return every **connected** connector that has
/// a readable EDID, sorted by name.
///
/// Per connector this is best-effort: an entry that is not a connector, one with
/// no `status`, a disconnected one, and one whose `edid` is empty (which is what
/// a connector reports when nothing has been read from it) all contribute
/// nothing rather than failing the scan.
///
/// # Errors
///
/// The **tree itself** is different. A machine with no DRM at all is a
/// legitimate state — a container, a headless server, or any non-Linux host
/// running these tests — so a `NotFound` is an empty list. Any *other* failure
/// to read the directory is a real fault (`/sys` not mounted as sysfs,
/// permissions, a truncated container mount) and is returned, because reporting
/// it as "no monitors found" would send a user looking for a hardware problem
/// they do not have.
pub fn scan(root: &Path) -> Result<Vec<DrmConnector>, io::Error> {
    let entries = match fs::read_dir(root.join("sys").join("class").join("drm")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        let Some(raw) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        // `card0`, `renderD128`, `version` and the `ttm` directory all live
        // beside the connectors; only a connector carries a `card<N>-` prefix.
        let Some(name) = strip_card_prefix(&raw) else {
            continue;
        };
        let Ok(status) = fs::read_to_string(dir.join("status")) else {
            continue;
        };
        if status.trim() != "connected" {
            continue;
        }
        let Ok(edid) = fs::read(dir.join("edid")) else {
            continue;
        };
        // A connector that has never been probed reports a zero-length `edid`,
        // and a truncated one cannot carry an identity. Both mean "no display
        // here", not a fault.
        if edid.len() < EDID_BLOCK_LEN {
            continue;
        }
        out.push(DrmConnector {
            is_internal: is_internal(name),
            name: name.to_owned(),
            edid,
            i2c: i2c_bus(&dir),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Strip the `card<N>-` prefix from a sysfs DRM entry name, returning `None` for
/// an entry that is not a connector.
///
/// `card0-DP-1` → `DP-1`. `card0`, `renderD128` and `version` → `None`.
fn strip_card_prefix(entry: &str) -> Option<&str> {
    let rest = entry.strip_prefix("card")?;
    // `card0-DP-1` splits into the card index and the connector name; a bare
    // `card0` has no `-` and is the device itself, not a connector.
    let (index, name) = rest.split_once('-')?;
    if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    (!name.is_empty()).then_some(name)
}

/// The connector's **type** token: its name with the trailing `-<index>`
/// removed.
///
/// `DP-1` → `DP`, `HDMI-A-2` → `HDMI-A`, `eDP-1` → `eDP`. Splitting on the
/// *last* hyphen rather than the first is load-bearing: `HDMI-A` and `DVI-D` are
/// two-token type names, and taking the first token would classify them as
/// `HDMI` and `DVI`. Nothing downstream cares today — neither is internal —
/// which is exactly why a rule that is right only for the types that happen not
/// to matter would go unnoticed until one did.
fn connector_type(name: &str) -> &str {
    match name.rsplit_once('-') {
        Some((head, index)) if !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()) => {
            head
        }
        _ => name,
    }
}

/// Whether a connector name (`card<N>-` prefix already stripped) names a
/// built-in panel.
fn is_internal(name: &str) -> bool {
    let ty = connector_type(name).to_ascii_lowercase();
    INTERNAL_TYPES.contains(&ty.as_str())
}

/// Resolve the I2C adapter index behind `<connector>/ddc`, or why there is none.
///
/// See the module docs for why this looks for the `i2c-dev/i2c-<N>` child rather
/// than reading the symlink's own target.
fn i2c_bus(connector: &Path) -> Result<u32, NoI2c> {
    let ddc = connector.join("ddc");
    if !ddc.exists() {
        return Err(NoI2c::NoDdcLink);
    }
    let Ok(entries) = fs::read_dir(ddc.join("i2c-dev")) else {
        return Err(NoI2c::NoI2cDev);
    };
    // Lowest rather than first: a driver may publish more than one child, and a
    // bus number chosen by directory order would vary between runs on one
    // machine.
    entries
        .flatten()
        .filter_map(|e| parse_i2c_index(e.file_name().to_str()?))
        .min()
        .ok_or(NoI2c::NoI2cDev)
}

/// Parse `i2c-7` into `7`. Anything else is `None`.
fn parse_i2c_index(entry: &str) -> Option<u32> {
    entry.strip_prefix("i2c-")?.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;

    /// [`scan`] for the cases where a readable tree is a precondition, not the
    /// thing under test.
    fn scanned(root: &Path) -> Vec<DrmConnector> {
        scan(root).expect("a readable drm tree")
    }

    /// The fixed 8-byte EDID header.
    const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

    /// A minimal but valid-shaped EDID block: the header every parser keys on,
    /// padded to one block. Identity parsing belongs to [`crate::id`] and has its
    /// own suite; this module only decides whether there are enough bytes to hand
    /// it. Built without indexing, so the test module stays clean under
    /// `indexing_slicing`.
    fn edid_block() -> Vec<u8> {
        let mut edid = Vec::with_capacity(EDID_BLOCK_LEN);
        edid.extend_from_slice(&HEADER);
        edid.resize(EDID_BLOCK_LEN, 0);
        edid
    }

    /// The connector at `index` in a scan result, or a failed assertion naming
    /// what was actually found.
    fn nth(found: &[DrmConnector], index: usize) -> &DrmConnector {
        let Some(connector) = found.get(index) else {
            let names: Vec<&str> = found.iter().map(|c| c.name.as_str()).collect();
            panic!("no connector at {index}; the scan found {names:?}");
        };
        connector
    }

    /// Builds a `<root>/sys/class/drm` fixture one connector at a time.
    struct Sysfs {
        dir: TempDir,
    }

    impl Sysfs {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            fs::create_dir_all(dir.path().join("sys/class/drm")).expect("drm dir");
            Sysfs { dir }
        }

        fn root(&self) -> &Path {
            self.dir.path()
        }

        fn drm(&self) -> PathBuf {
            self.dir.path().join("sys/class/drm")
        }

        /// A connector directory with `status` and `edid` written.
        fn connector(&self, entry: &str, status: &str, edid: &[u8]) -> PathBuf {
            let dir = self.drm().join(entry);
            fs::create_dir_all(&dir).expect("connector dir");
            fs::write(dir.join("status"), format!("{status}\n")).expect("status");
            fs::write(dir.join("edid"), edid).expect("edid");
            dir
        }

        /// Give a connector a `ddc` entry exposing `/dev/i2c-<index>`.
        ///
        /// A plain directory stands in for sysfs's symlink: `read_dir` cannot
        /// tell them apart, and creating a symlink on Windows needs a privilege
        /// CI does not grant — which is part of why this is driven through the
        /// `i2c-dev` child rather than `readlink`.
        fn with_i2c(connector: &Path, index: u32) {
            let dir = connector
                .join("ddc")
                .join("i2c-dev")
                .join(format!("i2c-{index}"));
            fs::create_dir_all(dir).expect("i2c-dev dir");
        }

        /// Give a connector a `ddc` entry with no `i2c-dev` child: the shape left
        /// behind when the `i2c-dev` module is not loaded.
        fn with_ddc_but_no_i2c_dev(connector: &Path) {
            fs::create_dir_all(connector.join("ddc").join("device")).expect("ddc dir");
        }
    }

    #[test]
    fn a_connected_external_connector_with_an_i2c_node_is_usable() {
        let sysfs = Sysfs::new();
        let dp = sysfs.connector("card0-DP-1", "connected", &edid_block());
        Sysfs::with_i2c(&dp, 7);

        let found = scanned(sysfs.root());

        assert_eq!(found.len(), 1);
        let dp = nth(&found, 0);
        assert_eq!(dp.name, "DP-1");
        assert_eq!(dp.i2c, Ok(7));
        assert!(!dp.is_internal);
        assert_eq!(dp.edid.len(), EDID_BLOCK_LEN);
    }

    #[test]
    fn a_disconnected_connector_contributes_nothing() {
        let sysfs = Sysfs::new();
        let dp = sysfs.connector("card0-DP-2", "disconnected", &edid_block());
        Sysfs::with_i2c(&dp, 3);

        assert!(scanned(sysfs.root()).is_empty());
    }

    /// An empty `edid` is what a connector reports before anything has been read
    /// from it, and a short one cannot carry an identity. Neither is a fault, so
    /// neither may become a fabricated display.
    #[test]
    fn an_empty_or_truncated_edid_contributes_nothing() {
        let sysfs = Sysfs::new();
        let empty = sysfs.connector("card0-DP-1", "connected", &[]);
        Sysfs::with_i2c(&empty, 1);
        let short = sysfs.connector("card0-DP-2", "connected", &[0u8; 64]);
        Sysfs::with_i2c(&short, 2);

        assert!(scanned(sysfs.root()).is_empty());
    }

    /// The two "no DDC" states are distinct because their remedies are: one is
    /// the driver's choice, the other is a kernel module the user can load.
    /// Collapsing them would leave `dujactl doctor` unable to say which applies.
    #[test]
    fn the_two_reasons_a_connector_has_no_i2c_bus_stay_apart() {
        let sysfs = Sysfs::new();
        sysfs.connector("card0-DP-1", "connected", &edid_block());
        let hdmi = sysfs.connector("card0-HDMI-A-1", "connected", &edid_block());
        Sysfs::with_ddc_but_no_i2c_dev(&hdmi);

        let found = scanned(sysfs.root());

        assert_eq!(found.len(), 2);
        assert_eq!(nth(&found, 0).name, "DP-1");
        assert_eq!(nth(&found, 0).i2c, Err(NoI2c::NoDdcLink));
        assert_eq!(nth(&found, 1).name, "HDMI-A-1");
        assert_eq!(nth(&found, 1).i2c, Err(NoI2c::NoI2cDev));
    }

    #[test]
    fn built_in_panel_types_are_flagged_and_external_ones_are_not() {
        for name in ["eDP-1", "LVDS-1", "DSI-1", "DPI-1"] {
            assert!(is_internal(name), "{name} names a built-in panel");
        }
        for name in ["DP-1", "HDMI-A-1", "DVI-D-1", "VGA-1", "Virtual-1"] {
            assert!(!is_internal(name), "{name} is not a built-in panel");
        }
    }

    /// `HDMI-A` and `DVI-D` are two-token type names, so the index must be split
    /// off the *end*. Splitting at the first hyphen would call them `HDMI` and
    /// `DVI` — harmless today, since neither is internal, which is exactly why it
    /// would go unnoticed until a two-token internal type appeared.
    #[test]
    fn the_type_token_is_split_off_the_index_not_the_first_hyphen() {
        assert_eq!(connector_type("DP-1"), "DP");
        assert_eq!(connector_type("eDP-1"), "eDP");
        assert_eq!(connector_type("HDMI-A-2"), "HDMI-A");
        assert_eq!(connector_type("DVI-D-10"), "DVI-D");
        // No trailing index at all: the whole name is the type.
        assert_eq!(connector_type("Virtual"), "Virtual");
        // A trailing token that is not a number is part of the type.
        assert_eq!(connector_type("HDMI-A"), "HDMI-A");
    }

    #[test]
    fn non_connector_entries_beside_the_connectors_are_ignored() {
        let sysfs = Sysfs::new();
        for entry in ["card0", "renderD128", "version", "ttm"] {
            fs::create_dir_all(sysfs.drm().join(entry)).expect("sibling");
        }
        // `card0` in particular is a real directory with real children; it is
        // rejected on its *name*, before any file is read.
        assert_eq!(strip_card_prefix("card0"), None);
        assert_eq!(strip_card_prefix("renderD128"), None);
        assert_eq!(strip_card_prefix("version"), None);
        assert_eq!(strip_card_prefix("cardX-DP-1"), None);
        assert_eq!(strip_card_prefix("card1-HDMI-A-2"), Some("HDMI-A-2"));

        assert!(scanned(sysfs.root()).is_empty());
    }

    /// Enumeration order must not depend on the order the filesystem hands back
    /// directory entries: a display's position in this list is what the app's
    /// slot assignment keys on.
    #[test]
    fn the_scan_is_sorted_by_connector_name() {
        let sysfs = Sysfs::new();
        for entry in ["card0-HDMI-A-1", "card0-DP-2", "card1-eDP-1", "card0-DP-1"] {
            sysfs.connector(entry, "connected", &edid_block());
        }

        let names: Vec<_> = scanned(sysfs.root()).into_iter().map(|c| c.name).collect();

        assert_eq!(names, ["DP-1", "DP-2", "HDMI-A-1", "eDP-1"]);
    }

    /// No DRM tree at all is an ordinary state — a container, a headless server,
    /// or any non-Linux host running this test — and must be an empty list, not a
    /// panic and not an error.
    #[test]
    fn a_machine_with_no_drm_tree_yields_an_empty_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(scanned(dir.path()).is_empty());
    }

    /// An unreadable tree is NOT the same answer as an absent one. Collapsing the
    /// two would report a mounting or permission fault as "no monitors found",
    /// which sends the user looking for a hardware problem they do not have.
    ///
    /// The fixture puts a regular file where the directory belongs, which is the
    /// one non-`NotFound` `read_dir` failure that reproduces identically on all
    /// three CI lanes.
    #[test]
    fn a_drm_path_that_is_not_a_directory_is_an_error_not_an_empty_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("sys/class")).expect("class dir");
        fs::write(dir.path().join("sys/class/drm"), "not a directory").expect("file");

        let err = scan(dir.path()).expect_err("a file is not a readable drm tree");

        assert_ne!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn only_a_well_formed_i2c_dev_entry_names_a_bus() {
        assert_eq!(parse_i2c_index("i2c-7"), Some(7));
        assert_eq!(parse_i2c_index("i2c-0"), Some(0));
        assert_eq!(parse_i2c_index("i2c-"), None);
        assert_eq!(parse_i2c_index("i2c-x"), None);
        assert_eq!(parse_i2c_index("7"), None);
        assert_eq!(parse_i2c_index("i2c-1-0037"), None);
    }

    /// A `ddc` entry can expose more than one `i2c-dev` child; picking the lowest
    /// is arbitrary, but it must be *deterministic*, because a bus number chosen
    /// by directory order would vary between runs on one machine.
    #[test]
    fn the_lowest_i2c_index_is_chosen_deterministically() {
        let sysfs = Sysfs::new();
        let dp = sysfs.connector("card0-DP-1", "connected", &edid_block());
        Sysfs::with_i2c(&dp, 9);
        Sysfs::with_i2c(&dp, 4);
        Sysfs::with_i2c(&dp, 12);

        assert_eq!(nth(&scanned(sysfs.root()), 0).i2c, Ok(4));
    }
}
