//! Pure scanning of the Linux backlight tree in sysfs, and the raw↔percent
//! conversion built on it.
//!
//! A Linux internal panel is controlled through `/sys/class/backlight/<dev>/`,
//! where `max_brightness` gives the hardware's step count and `brightness` is
//! the current level. Duja's [`PanelTransport`](crate::PanelTransport) contract
//! is percent-based (Windows WMI is), so this module owns both halves of the
//! translation and the choice of *which* device to drive when a machine exposes
//! several.
//!
//! Everything here is plain file reads under an **injected root** — `/` in
//! production, a `tempfile::TempDir` in tests — so it is
//! `cfg(any(test, target_os = "linux"))` and every rule below runs on all three
//! CI lanes. What is left for the Linux-only transport is the *write*: a logind
//! D-Bus call, or a direct write to `brightness` (see the transport for why
//! logind comes first).
//!
//! # Not every backlight device is a backlight
//!
//! `/sys/class/backlight` is a *class*, not a whitelist, and the
//! `ddcci-backlight` kernel module registers one entry in it **per external
//! monitor** — it drives DDC/CI monitors as if they were backlights. Those
//! entries are named `ddcci<N>` and report `type = raw`, which is the same kind
//! an ordinary `intel_backlight` reports, and `ddcci` sorts before `intel` — so
//! on an ordinary Intel laptop with `ddcci-dkms` installed, the "best" device by
//! kind and name is an **external monitor**. Duja would then pair it with the
//! internal panel's EDID identity and drive the wrong screen from the panel row.
//!
//! They are excluded by name, which is worth being uncomfortable about: this
//! project's standing rule is that a table of third-party names goes stale
//! silently. The difference is what the name identifies. `ddcci<N>` is a *kernel
//! module's* documented device naming, not a desktop's identity, and the
//! alternative — following `<dev>/device` and asking whether it lands on the
//! internal DRM connector — is a sysfs-layout claim this project cannot verify
//! without a Linux machine. `docs/debt.md` carries the principled version and
//! what it needs.
//!
//! # Which device, when there are several
//!
//! A laptop commonly exposes two or three: an ACPI one (`acpi_video0`), a
//! vendor one (`dell_backlight`, `asus-nb-wmi`), and the GPU's own
//! (`intel_backlight`, `amdgpu_bl0`). The kernel labels each in a `type` file as
//! `firmware`, `platform` or `raw`, and Duja prefers them in that order.
//!
//! The reason is agreement rather than capability. A `firmware` or `platform`
//! device routes the change through the path the machine's own brightness keys
//! and the desktop's power daemon already use, so all three stay in step. A
//! `raw` device pokes the GPU's registers directly, which works but leaves the
//! firmware holding a different idea of the level — the state where a brightness
//! key press jumps the panel back to somewhere Duja never set.
//!
//! (The `sysfs-class-backlight` ABI documentation is *believed* to state this
//! same preference order. Duja does not rest on that: the argument above is its
//! own. The claim is recorded as unverified because no kernel tree was read to
//! confirm it.)

use std::fs;
use std::path::{Path, PathBuf};

/// How a backlight device reaches the panel, from the kernel's `type` file.
///
/// Ordered: [`Firmware`](Self::Firmware) is preferred over
/// [`Platform`](Self::Platform), which is preferred over [`Raw`](Self::Raw). See
/// the module docs for why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum BacklightKind {
    /// A standard firmware interface (ACPI video). Preferred.
    Firmware,
    /// A vendor-specific platform interface.
    Platform,
    /// Direct GPU register access (`intel_backlight`, `amdgpu_bl0`).
    Raw,
}

impl BacklightKind {
    /// Parse the kernel's `type` file. An unknown or unreadable value is treated
    /// as [`Raw`](Self::Raw) — the least-preferred kind — so an unrecognised
    /// device is still usable but never wins over one Duja understands.
    fn parse(raw: &str) -> Self {
        match raw.trim() {
            "firmware" => BacklightKind::Firmware,
            "platform" => BacklightKind::Platform,
            _ => BacklightKind::Raw,
        }
    }
}

/// One backlight device: everything needed to read its level and to address it
/// for a write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Backlight {
    /// The device's directory name (`intel_backlight`, `acpi_video0`). This is
    /// also the name logind's `SetBrightness` takes as its second argument.
    pub name: String,
    /// The device's directory, with the injected root already applied. The
    /// transport writes `<dir>/brightness` through this, so a test fixture and a
    /// real device are addressed identically.
    pub dir: PathBuf,
    /// How this device reaches the panel.
    pub kind: BacklightKind,
    /// The hardware's highest raw level. Always non-zero: a device reporting
    /// `max_brightness = 0` cannot be driven and is skipped by [`scan`].
    pub max: u32,
    /// The current raw level, clamped to `max`.
    pub current: u32,
}

impl Backlight {
    /// The percentages this device can actually distinguish, ascending.
    ///
    /// There is deliberately no `percent()` beside this: [`current`](Self::current)
    /// is a snapshot from [`scan`], and a caller that wanted "the level now" and
    /// found a convenient method here would get the level at enumeration instead.
    /// The transport re-reads through [`read_level`] and converts with
    /// [`raw_to_percent`], which is the only correct order.
    pub fn levels(&self) -> Vec<u8> {
        levels(self.max)
    }
}

/// The device-name prefix `ddcci-backlight` registers external monitors under.
///
/// See the module docs for why this exclusion is by name and why that is not the
/// name-table pattern the project otherwise refuses.
const DDCCI_PREFIX: &str = "ddcci";

/// Scan `<root>/sys/class/backlight` and return every usable **panel** device,
/// **best first** (see [`BacklightKind`]), with a deterministic tie-break by name.
///
/// A device is usable when `max_brightness` parses and is non-zero and
/// `brightness` parses. Anything else contributes nothing: a machine with no
/// backlight at all — every desktop — yields an empty list, which is the
/// "graceful absence" the crate documents, not an error.
///
/// `ddcci<N>` entries are external monitors wearing a backlight's clothes and are
/// excluded outright; see the module docs.
pub(crate) fn scan(root: &Path) -> Vec<Backlight> {
    let Ok(entries) = fs::read_dir(root.join("sys").join("class").join("backlight")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        // An external monitor driven by `ddcci-backlight`. Excluded before the
        // ranking, not after: it reports `type = raw` like a GPU backlight and
        // `ddcci` sorts before `intel_backlight`, so left in it would *win* on an
        // ordinary laptop and be paired with the internal panel's identity.
        if name.starts_with(DDCCI_PREFIX) {
            continue;
        }
        let Some(max) = read_u32(&dir, "max_brightness") else {
            continue;
        };
        // A zero-step device has no range to drive and would make every percent
        // conversion a division by zero.
        if max == 0 {
            continue;
        }
        let Some(current) = read_u32(&dir, "brightness") else {
            continue;
        };
        let kind = fs::read_to_string(dir.join("type"))
            .map_or(BacklightKind::Raw, |raw| BacklightKind::parse(&raw));
        out.push(Backlight {
            name,
            dir,
            kind,
            max,
            // A device is free to report a level above its own maximum after a
            // firmware update or a resume; clamping here means no conversion
            // below has to defend against it.
            current: current.min(max),
        });
    }
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Read a sysfs attribute as a `u32`. Absent, unreadable, or not a number are
/// all the same answer: this device does not report it.
fn read_u32(dir: &Path, attr: &str) -> Option<u32> {
    fs::read_to_string(dir.join(attr)).ok()?.trim().parse().ok()
}

/// Re-read a device's current raw level, or `None` if the device has gone.
///
/// `brightness` and not `actual_brightness`: the first is the level the kernel
/// has been asked for, which is what a read-back after a write must return,
/// while the second is a hardware sample that can lag a fade or round
/// differently. The transport re-reads through this on every query rather than
/// reporting [`Backlight::current`], which is a snapshot from enumeration and
/// goes stale the moment a brightness key is pressed.
pub(crate) fn read_level(dir: &Path) -> Option<u32> {
    read_u32(dir, "brightness")
}

/// Convert a raw hardware level to a percentage, rounding to nearest.
///
/// `max` is non-zero for every [`Backlight`] [`scan`] returns; a zero passed
/// here anyway answers `0` rather than dividing.
pub(crate) fn raw_to_percent(raw: u32, max: u32) -> u8 {
    let level = u64::from(raw.min(max));
    let max = u64::from(max);
    // Round to nearest rather than truncating: with `max = 3`, truncation maps
    // the top step to 99% and no input to 100, so a panel at full brightness
    // would read back as not-quite-full forever.
    //
    // Written with checked/saturating operations rather than `*` and `/` so the
    // whole function is total under `clippy::arithmetic_side_effects`. The
    // divisor is the caller's `max`, and it is the **second** `checked_div` below
    // that turns `max = 0` into the documented `0`; this first one cannot fail
    // (dividing by the literal 2) and is written this way only so the lint sees
    // no bare operator.
    let Some(half) = max.checked_div(2) else {
        return 0;
    };
    let Some(percent) = level
        .saturating_mul(100)
        .saturating_add(half)
        .checked_div(max)
    else {
        return 0;
    };
    u8::try_from(percent.min(100)).unwrap_or(100)
}

/// Convert a percentage to a raw hardware level, rounding to nearest.
///
/// `percent` is clamped to `0..=100` by the caller; clamped again here so the
/// function is total.
pub(crate) fn percent_to_raw(percent: u8, max: u32) -> u32 {
    let percent = u64::from(percent.min(100));
    let raw = percent
        .saturating_mul(u64::from(max))
        .saturating_add(50)
        .checked_div(100)
        .unwrap_or(0);
    u32::try_from(raw).unwrap_or(max).min(max)
}

/// The percentages a device with `max` raw steps can actually distinguish,
/// ascending and deduplicated.
///
/// A device with more steps than percentage points reaches every percent, so the
/// answer is simply `0..=100`. A coarse one (`max_brightness = 3` is real: some
/// ACPI panels expose four levels) reaches only four, and reporting all 101 would
/// promise a precision the hardware does not have.
pub(crate) fn levels(max: u32) -> Vec<u8> {
    if max == 0 {
        return Vec::new();
    }
    if max >= 100 {
        return (0..=100).collect();
    }
    let mut out: Vec<u8> = (0..=max).map(|raw| raw_to_percent(raw, max)).collect();
    // Below 100 steps the gap between adjacent levels exceeds one percentage
    // point, so this removes nothing today. It is here so a later change to the
    // rounding cannot quietly produce a level list with repeats in it.
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    /// The device at `index` in a scan result, or a failed assertion naming what
    /// was actually found. Written without indexing so the test module stays
    /// clean under `indexing_slicing`.
    fn nth(found: &[Backlight], index: usize) -> &Backlight {
        let Some(device) = found.get(index) else {
            let names: Vec<&str> = found.iter().map(|d| d.name.as_str()).collect();
            panic!("no backlight device at {index}; the scan found {names:?}");
        };
        device
    }

    /// Builds a `<root>/sys/class/backlight` fixture one device at a time.
    struct Sysfs {
        dir: TempDir,
    }

    impl Sysfs {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            fs::create_dir_all(dir.path().join("sys/class/backlight")).expect("backlight dir");
            Sysfs { dir }
        }

        fn root(&self) -> &Path {
            self.dir.path()
        }

        fn device(&self, name: &str, kind: &str, max: &str, current: &str) {
            let dir = self.dir.path().join("sys/class/backlight").join(name);
            fs::create_dir_all(&dir).expect("device dir");
            fs::write(dir.join("type"), format!("{kind}\n")).expect("type");
            fs::write(dir.join("max_brightness"), format!("{max}\n")).expect("max");
            fs::write(dir.join("brightness"), format!("{current}\n")).expect("brightness");
        }
    }

    #[test]
    fn a_single_device_reports_its_level_as_a_percentage() {
        let sysfs = Sysfs::new();
        sysfs.device("intel_backlight", "raw", "96000", "48000");

        let found = scan(sysfs.root());

        assert_eq!(found.len(), 1);
        let device = nth(&found, 0);
        assert_eq!(device.name, "intel_backlight");
        assert_eq!(device.kind, BacklightKind::Raw);
        assert_eq!(device.max, 96_000);
        assert_eq!(raw_to_percent(device.current, device.max), 50);
    }

    /// The ordering is the whole point of reading `type`: on a laptop that
    /// exposes all three, the firmware device must come first so a Duja change
    /// and a brightness-key press stay in agreement.
    #[test]
    fn devices_are_ordered_firmware_then_platform_then_raw() {
        let sysfs = Sysfs::new();
        sysfs.device("intel_backlight", "raw", "100", "50");
        sysfs.device("acpi_video0", "firmware", "15", "7");
        sysfs.device("dell_backlight", "platform", "100", "50");

        let names: Vec<_> = scan(sysfs.root()).into_iter().map(|b| b.name).collect();

        assert_eq!(names, ["acpi_video0", "dell_backlight", "intel_backlight"]);
    }

    /// Two devices of the same kind must not swap places between runs — the
    /// first is the one Duja drives.
    #[test]
    fn devices_of_the_same_kind_break_ties_by_name() {
        let sysfs = Sysfs::new();
        sysfs.device("nvidia_wmi_ec_backlight", "firmware", "100", "10");
        sysfs.device("acpi_video0", "firmware", "100", "20");

        let names: Vec<_> = scan(sysfs.root()).into_iter().map(|b| b.name).collect();

        assert_eq!(names, ["acpi_video0", "nvidia_wmi_ec_backlight"]);
    }

    /// An unknown `type`, or none at all, must still be usable — just never
    /// preferred over a kind Duja recognises.
    #[test]
    fn an_unrecognised_type_is_usable_but_ranks_last() {
        let sysfs = Sysfs::new();
        sysfs.device("weird_backlight", "something-new", "100", "10");
        sysfs.device("acpi_video0", "firmware", "100", "20");
        // A device with no `type` file at all.
        let dir = sysfs.root().join("sys/class/backlight/no_type");
        fs::create_dir_all(&dir).expect("dir");
        fs::write(dir.join("max_brightness"), "100").expect("max");
        fs::write(dir.join("brightness"), "5").expect("brightness");

        let found = scan(sysfs.root());

        assert_eq!(found.len(), 3);
        assert_eq!(nth(&found, 0).name, "acpi_video0");
        assert!(
            found
                .iter()
                .skip(1)
                .all(|device| device.kind == BacklightKind::Raw)
        );
    }

    /// The hazard the `ddcci` exclusion exists for, in its realistic shape: an
    /// ordinary Intel laptop with `ddcci-dkms` installed and two monitors
    /// attached. Both `ddcci` entries are `raw`, the same kind as
    /// `intel_backlight`, and `ddcci` sorts first — so without the exclusion the
    /// panel row would drive an external monitor.
    #[test]
    fn ddcci_monitors_never_win_the_panel_slot() {
        let sysfs = Sysfs::new();
        sysfs.device("ddcci7", "raw", "100", "50");
        sysfs.device("ddcci9", "raw", "100", "50");
        sysfs.device("intel_backlight", "raw", "96000", "48000");

        let found = scan(sysfs.root());

        assert_eq!(found.len(), 1);
        assert_eq!(nth(&found, 0).name, "intel_backlight");
    }

    /// A desktop whose only backlight devices are `ddcci` monitors has no panel,
    /// and must report none rather than the first monitor.
    #[test]
    fn a_machine_with_only_ddcci_devices_reports_no_panel() {
        let sysfs = Sysfs::new();
        sysfs.device("ddcci4", "raw", "100", "50");

        assert!(scan(sysfs.root()).is_empty());
    }

    #[test]
    fn an_unusable_device_contributes_nothing() {
        let sysfs = Sysfs::new();
        // Zero steps: nothing to drive, and a divisor of zero downstream.
        sysfs.device("zero", "raw", "0", "0");
        // Unparseable attributes.
        sysfs.device("garbage", "raw", "lots", "5");
        sysfs.device("missing_current", "raw", "100", "");

        assert!(scan(sysfs.root()).is_empty());
    }

    /// A desktop has no backlight tree at all, and so does every non-Linux host
    /// running this test. That is the crate's documented graceful absence.
    #[test]
    fn a_machine_with_no_backlight_tree_yields_an_empty_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(scan(dir.path()).is_empty());
    }

    /// A device may report a level above its own maximum after a resume or a
    /// firmware update. Clamping at the source means no conversion has to
    /// defend against it, and the percentage never exceeds 100.
    #[test]
    fn a_level_above_the_maximum_is_clamped_at_the_source() {
        let sysfs = Sysfs::new();
        sysfs.device("intel_backlight", "raw", "100", "255");

        let found = scan(sysfs.root());

        let device = nth(&found, 0);
        assert_eq!(device.current, 100);
        assert_eq!(raw_to_percent(device.current, device.max), 100);
    }

    /// Truncating instead of rounding would make the top step of a coarse panel
    /// read back as 99%, so "set to 100" would never confirm.
    #[test]
    fn the_top_raw_step_is_a_hundred_percent_even_on_a_coarse_panel() {
        for max in [1, 3, 7, 15, 100, 96_000] {
            assert_eq!(raw_to_percent(max, max), 100, "max = {max}");
            assert_eq!(raw_to_percent(0, max), 0, "max = {max}");
        }
    }

    /// The two conversions must agree well enough that reading back what was
    /// written does not move the level. A percent the hardware can represent
    /// must survive the round trip exactly.
    #[test]
    fn a_representable_percent_survives_the_round_trip() {
        for max in [100u32, 255, 1000, 96_000] {
            for percent in 0..=100u8 {
                let raw = percent_to_raw(percent, max);
                assert_eq!(
                    raw_to_percent(raw, max),
                    percent,
                    "percent {percent} through max {max} (raw {raw})"
                );
                assert!(raw <= max);
            }
        }
    }

    /// The rounding in `percent_to_raw` is only *observable* below 100 steps,
    /// which is exactly the regime the round-trip test above does not cover. With
    /// `max = 3` — a real coarse ACPI panel, and the case `levels()` exists for —
    /// truncation would map 50% to step 1 (33%) instead of step 2 (67%), so a
    /// slider at half would land a third of the way up and stay there.
    #[test]
    fn rounding_is_observable_on_a_coarse_panel_and_is_pinned_there() {
        assert_eq!(percent_to_raw(50, 3), 2);
        assert_eq!(percent_to_raw(33, 3), 1);
        assert_eq!(percent_to_raw(67, 3), 2);
        assert_eq!(percent_to_raw(100, 3), 3);
        assert_eq!(percent_to_raw(0, 3), 0);
        // Truncation would give 1 here, and the whole-range round trip above
        // cannot see it because it never uses a max below 100.
        assert_ne!(percent_to_raw(50, 3), 1);
    }

    /// Every percent a coarse panel *reports* must survive the round trip, or the
    /// UI offers a level the hardware then refuses to confirm.
    #[test]
    fn a_coarse_panels_own_levels_survive_the_round_trip() {
        for max in [1u32, 2, 3, 4, 7, 15, 60, 99] {
            for percent in levels(max) {
                let raw = percent_to_raw(percent, max);
                assert_eq!(
                    raw_to_percent(raw, max),
                    percent,
                    "percent {percent} through max {max} (raw {raw})"
                );
            }
        }
    }

    /// A coarse panel reports only the percentages it can reach, so the UI does
    /// not promise precision the hardware lacks.
    #[test]
    fn a_coarse_panel_reports_only_the_levels_it_can_reach() {
        assert_eq!(levels(3), [0, 33, 67, 100]);
        assert_eq!(levels(1), [0, 100]);
        assert_eq!(levels(4), [0, 25, 50, 75, 100]);
        assert_eq!(levels(0), Vec::<u8>::new());
    }

    /// With at least as many steps as percentage points, every percent is
    /// reachable, so the level list is the full range rather than a sampled one.
    #[test]
    fn a_fine_panel_reaches_every_percent() {
        for max in [100u32, 101, 255, 96_000] {
            let levels = levels(max);
            assert_eq!(levels.len(), 101, "max = {max}");
            assert_eq!(levels.first(), Some(&0), "max = {max}");
            assert_eq!(levels.last(), Some(&100), "max = {max}");
        }
    }

    /// `Backlight::levels` is what the transport reports to the UI, so it must be
    /// the *device's* step count and not a constant. Asserted through a scanned
    /// device rather than the free function, because the wiring between the two
    /// is the part that can be got wrong.
    #[test]
    fn a_scanned_device_reports_the_levels_its_own_step_count_allows() {
        let sysfs = Sysfs::new();
        sysfs.device("acpi_video0", "firmware", "3", "1");

        let found = scan(sysfs.root());

        assert_eq!(nth(&found, 0).levels(), [0, 33, 67, 100]);
    }

    /// The level moves under Duja's feet: a brightness key, a power-profile
    /// daemon, a lid-open event. `read_level` is how the transport re-reads it,
    /// and reporting [`Backlight::current`] instead would show the user a value
    /// captured at enumeration as though it were the truth.
    #[test]
    fn read_level_sees_a_change_made_behind_our_back() {
        let sysfs = Sysfs::new();
        sysfs.device("intel_backlight", "raw", "100", "40");
        let found = scan(sysfs.root());
        let device = nth(&found, 0);
        assert_eq!(device.current, 40);

        fs::write(device.dir.join("brightness"), "90").expect("someone else writes");

        assert_eq!(read_level(&device.dir), Some(90));
        // The snapshot is deliberately NOT updated in place; it is a value from
        // enumeration, and this is the whole reason the transport re-reads.
        assert_eq!(device.current, 40);
    }

    #[test]
    fn levels_are_ascending_and_unique() {
        for max in [1u32, 2, 3, 5, 9, 15, 60, 99, 100, 4096] {
            let levels = levels(max);
            assert!(
                levels
                    .windows(2)
                    .all(|pair| matches!(pair, [low, high] if low < high)),
                "max = {max} produced {levels:?}"
            );
        }
    }
}
