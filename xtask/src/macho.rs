//! Just enough Mach-O to read a binary's architectures and its minimum OS.
//!
//! # Why parse this rather than shell out to `otool`
//!
//! The bundle's `LSMinimumSystemVersion` is a claim about the *binary*, and only
//! the build honours it: nothing stops `cargo build` from producing a slice
//! targeting 10.12 that then goes into a bundle advertising 11.0. Pinning the
//! plist constant against the workflow's `MACOSX_DEPLOYMENT_TARGET` — as
//! [`crate::bundle`]'s tests do — pins two *string literals* to each other, and
//! neither of them is what reaches the compiler. A maintainer packaging locally
//! never sets that variable at all.
//!
//! So the check has to be on the artifact. `otool -l | awk` would do it, but
//! only where `otool` exists, which means only inside the release workflow and
//! never on the path a maintainer actually runs. Reading the header is a few
//! dozen bytes of little-endian integers; doing it here makes the check run
//! **identically locally and in CI**, and makes it unit-testable on every lane
//! against synthetic binaries.
//!
//! Deliberately partial: this reads the fat header, each slice's CPU type, and
//! the load command that carries the macOS deployment target. It is not a Mach-O
//! library and should not grow into one.
//!
//! # What constrains the constants below — and what does not
//!
//! The unit tests build their own synthetic binaries, and those builders import
//! the **same** constants the parser compares against. So a wrong
//! `LC_BUILD_VERSION`, `MH_MAGIC_64`, `CPU_TYPE_*` or field offset would leave
//! the suite green while making [`slices`] reject every real universal binary —
//! and since this runs inside `dist`'s packaging step, that direction **blocks
//! releases**. The tests constrain the *logic*: that the walk steps over
//! unrelated load commands, that a differing floor is visible, that truncation
//! is an error rather than a panic.
//!
//! Two things constrain the constants themselves. Every value here was read off
//! Apple's `cctools` (`fat.h`, `mach-o/loader.h`, `libstuff/ofile.c`) and dyld,
//! and is cited at its definition. And the release workflow's
//! `workflow_dispatch` dry run feeds this parser a **real `lipo` output** — a
//! false reject fails the packaging step loudly, which is how a wrong constant
//! would surface. Checking in a captured real fat header as a byte fixture would
//! close the gap properly; it needs a Mac to produce one, and is recorded in
//! `docs/debt.md`.
//!
//! # Layout, for the constants below
//!
//! A universal file starts with a **big-endian** fat header (`magic`,
//! `nfat_arch`) followed by one 20-byte `fat_arch` record per slice
//! (`cputype`, `cpusubtype`, `offset`, `size`, `align`). Each slice is a
//! complete thin Mach-O: a 32-byte **little-endian** header (`magic`,
//! `cputype`, …, `ncmds`, `sizeofcmds`, …) followed by `ncmds` load commands,
//! each starting with its own `cmd` and `cmdsize`. A plain thin binary is the
//! same thing without the fat wrapper.

/// `FAT_MAGIC` — a universal binary, headers big-endian.
const FAT_MAGIC: u32 = 0xcafe_babe;
/// `FAT_MAGIC_64` — the 64-bit fat format (8-byte offsets/sizes).
const FAT_MAGIC_64: u32 = 0xcafe_babf;
/// `MH_MAGIC_64` — a 64-bit thin Mach-O, headers little-endian.
const MH_MAGIC_64: u32 = 0xfeed_facf;

/// Bytes in a `fat_arch` record (5 × `u32`).
const FAT_ARCH_SIZE: usize = 20;
/// Bytes in a `fat_arch_64` record: `cputype`/`cpusubtype` (2 × `u32`),
/// `offset`/`size` (2 × `u64`), `align`/`reserved` (2 × `u32`). `reserved` is a
/// real field, not padding, and `lipo` does not initialise it — so nothing here
/// may assume it is zero.
const FAT_ARCH_64_SIZE: usize = 32;
/// Bytes in a `mach_header_64`.
const MACH_HEADER_64_SIZE: usize = 32;

/// `LC_BUILD_VERSION` — carries `platform`, `minos`, `sdk`. What every current
/// toolchain emits.
const LC_BUILD_VERSION: u32 = 0x32;
/// `PLATFORM_MACOS`. A binary may carry more than one `LC_BUILD_VERSION` (a
/// zippered one has macOS *and* Mac Catalyst), so the platform is checked rather
/// than assumed — otherwise the first command wins and the answer is whichever
/// platform the linker happened to emit first.
const PLATFORM_MACOS: u32 = 1;
/// `LC_VERSION_MIN_MACOSX` — the predecessor, `version` then `sdk`. Read too, so
/// a slice built by an older toolchain reports a version rather than "absent".
const LC_VERSION_MIN_MACOSX: u32 = 0x24;

/// `CPU_TYPE_X86_64`.
const CPU_TYPE_X86_64: u32 = 0x0100_0007;
/// `CPU_TYPE_ARM64`.
const CPU_TYPE_ARM64: u32 = 0x0100_000c;

/// One architecture inside a binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Slice {
    /// The architecture name `lipo` would print (`arm64`, `x86_64`, or
    /// `cputype-<n>` for anything else).
    pub(crate) arch: String,
    /// The deployment target as `X.Y` (or `X.Y.Z` when the patch is non-zero),
    /// or `None` if the slice carries neither version load command.
    pub(crate) min_os: Option<String>,
}

/// Read every architecture in `bytes`, with the minimum OS each was built for.
///
/// Accepts a universal binary or a plain 64-bit thin one.
///
/// # Errors
/// Returns a message if the magic is unrecognised or the file is truncated
/// part-way through a header — both of which mean the caller was handed
/// something that is not the binary it thinks it is.
pub(crate) fn slices(bytes: &[u8]) -> Result<Vec<Slice>, String> {
    match read_u32(bytes, 0, Endian::Big)? {
        FAT_MAGIC | FAT_MAGIC_64 => fat_slices(bytes),
        _ => Ok(vec![thin_slice(bytes, 0)?]),
    }
}

/// Byte order of a header. The fat wrapper is always big-endian; the thin
/// headers inside it are little-endian on every architecture Duja ships.
#[derive(Debug, Clone, Copy)]
enum Endian {
    /// Network order — the fat header.
    Big,
    /// Host order for `arm64`/`x86_64` — the thin headers.
    Little,
}

/// Walk the fat header's arch table and read each slice it points at.
fn fat_slices(bytes: &[u8]) -> Result<Vec<Slice>, String> {
    let is_64 = read_u32(bytes, 0, Endian::Big)? == FAT_MAGIC_64;
    let count = read_u32(bytes, 4, Endian::Big)? as usize;
    let entry = if is_64 {
        FAT_ARCH_64_SIZE
    } else {
        FAT_ARCH_SIZE
    };
    let mut out = Vec::new();
    for index in 0..count {
        let base = index
            .checked_mul(entry)
            .and_then(|n| n.checked_add(8))
            .ok_or("fat header arch table overflows")?;
        // `offset` is the 3rd field in both record layouts, but it is 8 bytes
        // wide in the 64-bit one.
        let offset = if is_64 {
            usize::try_from(read_u64(bytes, add(base, 8)?, Endian::Big)?)
                .map_err(|_| "slice offset does not fit in this host's usize".to_owned())?
        } else {
            read_u32(bytes, add(base, 8)?, Endian::Big)? as usize
        };
        out.push(thin_slice(bytes, offset)?);
    }
    Ok(out)
}

/// Read the thin Mach-O that starts at `start`.
fn thin_slice(bytes: &[u8], start: usize) -> Result<Slice, String> {
    let magic = read_u32(bytes, start, Endian::Little)?;
    if magic != MH_MAGIC_64 {
        return Err(format!(
            "not a 64-bit Mach-O at offset {start} (magic {magic:#x})"
        ));
    }
    let cpu_type = read_u32(bytes, add(start, 4)?, Endian::Little)?;
    let ncmds = read_u32(bytes, add(start, 16)?, Endian::Little)? as usize;

    let mut cursor = add(start, MACH_HEADER_64_SIZE)?;
    let mut min_os = None;
    for _ in 0..ncmds {
        let cmd = read_u32(bytes, cursor, Endian::Little)?;
        let size = read_u32(bytes, add(cursor, 4)?, Endian::Little)? as usize;
        if size < 8 {
            return Err(format!("load command at {cursor} has size {size}"));
        }
        // `minos` is the 4th word of LC_BUILD_VERSION (after `cmd`, `cmdsize`,
        // `platform`) and the 3rd of the older LC_VERSION_MIN_MACOSX (after
        // `cmd`, `cmdsize`); both encode it the same way. Note the offsets are
        // *not* interchangeable — reading one word further gets `sdk`, which
        // would compare the build machine's SDK against the advertised floor.
        //
        // The `size` guards keep every read inside the command that claims it.
        // `read_u32` bounds-checks against the *file*, not the command, so
        // without them a command whose `cmdsize` lies would read its
        // neighbour's bytes — and that is not academic: `PLATFORM_MACOS` is 1,
        // which is also `LC_SEGMENT`, so a truncated build-version command
        // followed by a segment would pass the platform check and report the
        // segment's `cmdsize` as a version. Silently wrong is the one outcome
        // this parser must not have.
        let packed_at = match cmd {
            // Only the macOS entry: a zippered binary also carries a Mac
            // Catalyst one, and taking whichever came first would report a
            // different platform's floor.
            LC_BUILD_VERSION
                if size >= 16
                    && read_u32(bytes, add(cursor, 8)?, Endian::Little)? == PLATFORM_MACOS =>
            {
                Some(add(cursor, 12)?)
            }
            LC_VERSION_MIN_MACOSX if size >= 12 => Some(add(cursor, 8)?),
            _ => None,
        };
        if let Some(at) = packed_at {
            min_os = Some(format_version(read_u32(bytes, at, Endian::Little)?));
            break;
        }
        cursor = add(cursor, size)?;
    }
    Ok(Slice {
        arch: arch_name(cpu_type),
        min_os,
    })
}

/// The name `lipo -archs` prints for a CPU type.
fn arch_name(cpu_type: u32) -> String {
    match cpu_type {
        CPU_TYPE_ARM64 => "arm64".to_owned(),
        CPU_TYPE_X86_64 => "x86_64".to_owned(),
        other => format!("cputype-{other}"),
    }
}

/// Unpack Apple's `xxxx.yy.zz` nibble-packed version, printed the way `otool`
/// does: the patch component is elided when zero, so `11.0.0` reads `11.0` and
/// compares equal to a `MACOSX_DEPLOYMENT_TARGET` of `11.0`.
fn format_version(packed: u32) -> String {
    let major = packed >> 16;
    let minor = (packed >> 8) & 0xff;
    let patch = packed & 0xff;
    if patch == 0 {
        format!("{major}.{minor}")
    } else {
        format!("{major}.{minor}.{patch}")
    }
}

/// `a + b`, as an error rather than a panic or a wrap.
fn add(a: usize, b: usize) -> Result<usize, String> {
    a.checked_add(b)
        .ok_or_else(|| "Mach-O offset overflows".to_owned())
}

/// Read a `u32` at `offset`, reporting truncation rather than panicking.
fn read_u32(bytes: &[u8], offset: usize, endian: Endian) -> Result<u32, String> {
    let end = add(offset, 4)?;
    let chunk: [u8; 4] = bytes
        .get(offset..end)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| format!("truncated Mach-O: no 4 bytes at offset {offset}"))?;
    Ok(match endian {
        Endian::Big => u32::from_be_bytes(chunk),
        Endian::Little => u32::from_le_bytes(chunk),
    })
}

/// Read a `u64` at `offset`, reporting truncation rather than panicking.
fn read_u64(bytes: &[u8], offset: usize, endian: Endian) -> Result<u64, String> {
    let end = add(offset, 8)?;
    let chunk: [u8; 8] = bytes
        .get(offset..end)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| format!("truncated Mach-O: no 8 bytes at offset {offset}"))?;
    Ok(match endian {
        Endian::Big => u64::from_be_bytes(chunk),
        Endian::Little => u64::from_le_bytes(chunk),
    })
}

/// Synthetic Mach-O files, so the code that *consumes* [`slices`] — the
/// packaging check in [`crate::dist`] — can be tested on a host with no `lipo`,
/// no Xcode and no Mach-O binaries at all.
///
/// Hand-built rather than checked in as fixtures: the bytes are the
/// specification, and writing them out is what makes the parser's field offsets
/// reviewable against Apple's headers.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::{
        CPU_TYPE_ARM64, CPU_TYPE_X86_64, FAT_ARCH_SIZE, FAT_MAGIC, LC_BUILD_VERSION, MH_MAGIC_64,
        PLATFORM_MACOS,
    };

    /// Pack `X.Y.Z` the way the linker does.
    pub(crate) fn packed(major: u32, minor: u32, patch: u32) -> u32 {
        (major << 16) | (minor << 8) | patch
    }

    /// The CPU type for an arch name this crate knows.
    fn cpu_type_of(arch: &str) -> u32 {
        match arch {
            "arm64" => CPU_TYPE_ARM64,
            "x86_64" => CPU_TYPE_X86_64,
            other => panic!("unknown fixture arch {other}"),
        }
    }

    /// A universal binary whose slices are `(arch name, X.Y.Z)`.
    ///
    /// The shape `lipo` produces, so a caller can build both the artifact the
    /// packaging check must accept and each one it must refuse.
    pub(crate) fn universal(parts: &[(&str, (u32, u32, u32))]) -> Vec<u8> {
        let thin_parts: Vec<Vec<u8>> = parts
            .iter()
            .map(|(arch, (major, minor, patch))| {
                thin(
                    cpu_type_of(arch),
                    LC_BUILD_VERSION,
                    packed(*major, *minor, *patch),
                )
            })
            .collect();
        fat(&thin_parts)
    }

    /// An SDK version distinct from every `minos` a fixture uses.
    ///
    /// Load-bearing: when `sdk` and `minos` hold the same bytes, reading one
    /// word past `minos` returns the right answer by accident and the offset is
    /// pinned by nothing. That mistake would compare the build machine's SDK
    /// against the advertised floor and refuse to package **every** release.
    const FIXTURE_SDK: (u32, u32, u32) = (26, 3, 0);

    /// A thin 64-bit Mach-O with one `LC_BUILD_VERSION` (plus a filler load
    /// command in front of it, so the walk has to actually step over one).
    pub(crate) fn thin(cpu_type: u32, cmd: u32, version: u32) -> Vec<u8> {
        let sdk = packed(FIXTURE_SDK.0, FIXTURE_SDK.1, FIXTURE_SDK.2);
        let mut out = Vec::new();
        out.extend_from_slice(&MH_MAGIC_64.to_le_bytes()); // magic
        out.extend_from_slice(&cpu_type.to_le_bytes()); // cputype
        out.extend_from_slice(&0u32.to_le_bytes()); // cpusubtype
        out.extend_from_slice(&2u32.to_le_bytes()); // filetype
        out.extend_from_slice(&2u32.to_le_bytes()); // ncmds
        out.extend_from_slice(&40u32.to_le_bytes()); // sizeofcmds
        out.extend_from_slice(&0u32.to_le_bytes()); // flags
        out.extend_from_slice(&0u32.to_le_bytes()); // reserved

        // Filler: LC_SEGMENT_64, 16 bytes, nothing we read.
        out.extend_from_slice(&0x19u32.to_le_bytes());
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&[0u8; 8]);

        out.extend_from_slice(&cmd.to_le_bytes());
        if cmd == LC_BUILD_VERSION {
            out.extend_from_slice(&24u32.to_le_bytes()); // cmdsize
            out.extend_from_slice(&PLATFORM_MACOS.to_le_bytes()); // platform
            out.extend_from_slice(&version.to_le_bytes()); // minos
            out.extend_from_slice(&sdk.to_le_bytes()); // sdk
            out.extend_from_slice(&0u32.to_le_bytes()); // ntools
        } else {
            out.extend_from_slice(&16u32.to_le_bytes()); // cmdsize
            out.extend_from_slice(&version.to_le_bytes()); // version
            out.extend_from_slice(&sdk.to_le_bytes()); // sdk
        }
        out
    }

    /// A thin binary carrying two `LC_BUILD_VERSION`s, another platform's
    /// first and the macOS one after it — the zippered shape, where taking the
    /// first version command reports the wrong platform's floor.
    pub(crate) fn zippered(cpu_type: u32, other_platform: u32, other: u32, macos: u32) -> Vec<u8> {
        let sdk = packed(FIXTURE_SDK.0, FIXTURE_SDK.1, FIXTURE_SDK.2);
        let mut out = Vec::new();
        out.extend_from_slice(&MH_MAGIC_64.to_le_bytes());
        out.extend_from_slice(&cpu_type.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // cpusubtype
        out.extend_from_slice(&2u32.to_le_bytes()); // filetype
        out.extend_from_slice(&2u32.to_le_bytes()); // ncmds
        out.extend_from_slice(&48u32.to_le_bytes()); // sizeofcmds
        out.extend_from_slice(&0u32.to_le_bytes()); // flags
        out.extend_from_slice(&0u32.to_le_bytes()); // reserved
        for (platform, version) in [(other_platform, other), (PLATFORM_MACOS, macos)] {
            out.extend_from_slice(&LC_BUILD_VERSION.to_le_bytes());
            out.extend_from_slice(&24u32.to_le_bytes());
            out.extend_from_slice(&platform.to_le_bytes());
            out.extend_from_slice(&version.to_le_bytes());
            out.extend_from_slice(&sdk.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
        }
        out
    }

    /// A `u32` from a `usize` that a test fixture guarantees is small.
    fn small(n: usize) -> u32 {
        u32::try_from(n).expect("fixture value fits in u32")
    }

    /// A universal binary over the given thin slices.
    pub(crate) fn fat(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&FAT_MAGIC.to_be_bytes());
        out.extend_from_slice(&small(parts.len()).to_be_bytes());
        // Table first, then the slices, page-aligned the way `lipo` does — so
        // the parser has to follow the offsets rather than assume adjacency.
        let table_end = parts.len().saturating_mul(FAT_ARCH_SIZE).saturating_add(8);
        let mut offset = table_end.next_multiple_of(0x4000);
        let mut offsets = Vec::new();
        for part in parts {
            offsets.push(offset);
            offset = offset.saturating_add(part.len()).next_multiple_of(0x4000);
        }
        for (part, &at) in parts.iter().zip(offsets.iter()) {
            let cpu_type: [u8; 4] = part
                .get(4..8)
                .and_then(|s| s.try_into().ok())
                .expect("fixture slice has a cputype");
            out.extend_from_slice(&u32::from_le_bytes(cpu_type).to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes()); // cpusubtype
            out.extend_from_slice(&small(at).to_be_bytes());
            out.extend_from_slice(&small(part.len()).to_be_bytes());
            out.extend_from_slice(&14u32.to_be_bytes()); // align
        }
        for (part, &at) in parts.iter().zip(offsets.iter()) {
            out.resize(at, 0);
            out.extend_from_slice(part);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{self, fat, packed, thin};
    use super::*;

    /// Two slices, both built at the advertised floor — the case the packaging
    /// check must accept. (Slice order here is arm64-first for readability;
    /// `lipo` sorts by CPU type, so a real one lists `x86_64` first. The parser
    /// reports whatever order the fat table gives, which is why the assertions
    /// below are positional rather than a set.)
    #[test]
    fn a_universal_binary_reports_both_architectures_and_their_floors() {
        let bytes = fat(&[
            thin(CPU_TYPE_ARM64, LC_BUILD_VERSION, packed(11, 0, 0)),
            thin(CPU_TYPE_X86_64, LC_BUILD_VERSION, packed(11, 0, 0)),
        ]);
        assert_eq!(
            slices(&bytes).expect("parse"),
            vec![
                Slice {
                    arch: "arm64".to_owned(),
                    min_os: Some("11.0".to_owned())
                },
                Slice {
                    arch: "x86_64".to_owned(),
                    min_os: Some("11.0".to_owned())
                },
            ]
        );
    }

    /// The failure this parser exists to catch: an `x86_64` slice built without
    /// `MACOSX_DEPLOYMENT_TARGET`, which rustc defaults to an older release —
    /// silently, inside a bundle whose plist claims 11.0.
    #[test]
    fn a_slice_built_at_a_different_floor_is_visible() {
        let bytes = fat(&[
            thin(CPU_TYPE_ARM64, LC_BUILD_VERSION, packed(11, 0, 0)),
            thin(CPU_TYPE_X86_64, LC_BUILD_VERSION, packed(10, 12, 0)),
        ]);
        let read = slices(&bytes).expect("parse");
        assert_eq!(
            read.iter()
                .filter(|s| s.min_os.as_deref() == Some("11.0"))
                .count(),
            1
        );
        assert!(read.iter().any(|s| s.min_os.as_deref() == Some("10.12")));
    }

    #[test]
    fn a_thin_binary_is_read_as_a_single_slice() {
        let bytes = thin(CPU_TYPE_ARM64, LC_BUILD_VERSION, packed(11, 0, 0));
        assert_eq!(
            slices(&bytes).expect("parse"),
            vec![Slice {
                arch: "arm64".to_owned(),
                min_os: Some("11.0".to_owned())
            }]
        );
    }

    /// The predecessor load command, so a slice from an older toolchain reports
    /// its floor rather than reading as "no version at all" — which would make
    /// the packaging check pass by accident.
    #[test]
    fn the_older_version_load_command_is_read_too() {
        let bytes = thin(CPU_TYPE_X86_64, LC_VERSION_MIN_MACOSX, packed(10, 13, 0));
        assert_eq!(
            slices(&bytes)
                .expect("parse")
                .first()
                .and_then(|s| s.min_os.clone()),
            Some("10.13".to_owned())
        );
    }

    /// A zippered binary carries a second `LC_BUILD_VERSION` for Mac Catalyst,
    /// and Apple's linker may emit it first. Taking whichever version command
    /// came first would report *that* platform's floor — a different number,
    /// silently, with nothing to distinguish it from the right one.
    #[test]
    fn a_second_platforms_build_version_is_not_mistaken_for_the_macos_one() {
        // PLATFORM_MACCATALYST is 6; its floor is unrelated to the macOS one.
        let bytes = fixtures::zippered(CPU_TYPE_ARM64, 6, packed(14, 0, 0), packed(11, 0, 0));
        assert_eq!(
            slices(&bytes)
                .expect("parse")
                .first()
                .and_then(|s| s.min_os.clone()),
            Some("11.0".to_owned())
        );
    }

    /// `sdk` sits one word past `minos` in `LC_BUILD_VERSION` and one past
    /// `version` in its predecessor. Reading it instead would compare the *build
    /// machine's* SDK against the advertised floor and refuse every release, so
    /// the fixtures give it a value no test expects.
    #[test]
    fn the_sdk_version_is_never_read_as_the_minimum() {
        for cmd in [LC_BUILD_VERSION, LC_VERSION_MIN_MACOSX] {
            let bytes = fixtures::thin(CPU_TYPE_ARM64, cmd, packed(11, 0, 0));
            assert_eq!(
                slices(&bytes)
                    .expect("parse")
                    .first()
                    .and_then(|s| s.min_os.clone()),
                Some("11.0".to_owned()),
                "cmd {cmd:#x} read the wrong word"
            );
        }
    }

    /// A load command whose `cmdsize` is too small to hold the field being
    /// read must be skipped, not read past. `PLATFORM_MACOS` is 1 and
    /// `LC_SEGMENT` is also 1, so the neighbouring command's header would
    /// otherwise satisfy the platform check and its `cmdsize` would be reported
    /// as a version.
    #[test]
    fn a_load_command_that_lies_about_its_size_is_not_read_past() {
        let mut out = Vec::new();
        out.extend_from_slice(&MH_MAGIC_64.to_le_bytes());
        out.extend_from_slice(&CPU_TYPE_ARM64.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // cpusubtype
        out.extend_from_slice(&2u32.to_le_bytes()); // filetype
        out.extend_from_slice(&2u32.to_le_bytes()); // ncmds
        out.extend_from_slice(&24u32.to_le_bytes()); // sizeofcmds
        out.extend_from_slice(&0u32.to_le_bytes()); // flags
        out.extend_from_slice(&0u32.to_le_bytes()); // reserved
        // A build-version command claiming to be 8 bytes long...
        out.extend_from_slice(&LC_BUILD_VERSION.to_le_bytes());
        out.extend_from_slice(&8u32.to_le_bytes());
        // ...immediately followed by LC_SEGMENT (1), which is also the value of
        // PLATFORM_MACOS, with a cmdsize that would read as version 0.16.
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&[0u8; 8]);

        assert_eq!(
            slices(&out)
                .expect("parse")
                .first()
                .and_then(|s| s.min_os.clone()),
            None,
            "a short command must not be read past into its neighbour"
        );
    }

    #[test]
    fn a_non_zero_patch_component_is_kept() {
        assert_eq!(format_version(packed(11, 7, 2)), "11.7.2");
        assert_eq!(format_version(packed(26, 1, 0)), "26.1");
    }

    /// Truncation is an error, never a panic and never a wrong answer — this
    /// parser runs against whatever `lipo` wrote, including a half-written file
    /// from an interrupted build.
    #[test]
    fn a_truncated_file_is_an_error_rather_than_a_panic() {
        let full = fat(&[thin(CPU_TYPE_ARM64, LC_BUILD_VERSION, packed(11, 0, 0))]);
        for cut in [0, 1, 4, 7, 9, 20, full.len() / 2] {
            let err = slices(full.get(..cut).expect("prefix")).expect_err("truncated");
            assert!(!err.is_empty(), "cut {cut} produced an empty message");
        }
    }

    #[test]
    fn a_file_that_is_not_mach_o_at_all_is_rejected() {
        let err = slices(b"#!/bin/sh\necho hello\n").expect_err("shell script");
        assert!(err.contains("not a 64-bit Mach-O"), "{err}");
    }
}
