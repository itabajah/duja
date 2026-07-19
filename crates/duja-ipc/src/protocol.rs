//! The versioned request/response protocol: message types, the wire envelope,
//! and strict on-decode validation.
//!
//! # Envelope
//!
//! Every frame body is a JSON object carrying the protocol version and one
//! message: `{"v":2,"request":<Request>}` or `{"v":2,"response":<Response>}`.
//! The version is checked on decode; a mismatch is
//! [`IpcError::UnsupportedVersion`] rather than a silent misparse.
//!
//! # Unknown-field asymmetry
//!
//! [`Request`] is decoded with `deny_unknown_fields` at every level (the
//! envelope, the externally-tagged variant map, and each variant's fields):
//! the server is strict about what it accepts, so a typo or an injected extra
//! field is rejected rather than ignored. [`Response`] is decoded leniently
//! (unknown fields ignored) so a newer server can add response fields without
//! breaking an older client — forward compatibility on the read-back path.
//!
//! # Validation
//!
//! Beyond JSON shape, [`Request::validate`] / [`Response::validate`] enforce the
//! semantic invariants the transport relies on: brightness percentages are
//! `0..=100` and display ids match the `[A-Za-z0-9#-]` charset that
//! [`duja_core::id::StableDisplayId`] emits, length `1..=64`. The `read_*`
//! wrappers in the crate root run these automatically.

use serde::{Deserialize, Serialize};

use duja_core::model::{DisplayKind, DisplaySnapshot, Feature};

use crate::frame::IpcError;

/// The protocol version this build speaks. Bumped on any breaking wire change.
///
/// v2 (#67) removed `DisplayKindDto::SoftwareOnly` and added the required
/// [`DisplayInfo::software_only`] field. Because [`DisplayInfo`] is
/// `deny_unknown_fields`, that is breaking in BOTH directions (a v1 client omits
/// `software_only` / may send the removed variant; a v1 app rejects the new
/// field), so the bump is a hard wall: a v1 peer is rejected with
/// [`IpcError::UnsupportedVersion`] instead of silently misparsing.
pub const PROTOCOL_VERSION: u16 = 2;

/// The maximum length, in characters, of a display id on the wire.
pub const ID_MAX_LEN: usize = 64;

/// A command sent from a client (`dujactl` or a second app instance) to the
/// running app.
///
/// Externally tagged and `deny_unknown_fields` for server-side strictness (see
/// the module docs): `ListDisplays` and `ShowFlyout` serialize as the bare
/// strings `"list_displays"` / `"show_flyout"`, the data-carrying variants as a
/// single-key object such as `{"set_brightness":{"id":"…","pct":50}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Enumerate every known display and its current state.
    ListDisplays,
    /// Read one display's current unified brightness level.
    GetBrightness {
        /// The target display's stable id.
        id: String,
    },
    /// Set one display's unified brightness level, in percent.
    SetBrightness {
        /// The target display's stable id.
        id: String,
        /// The desired level, `0..=100`.
        pct: u8,
    },
    /// Ask the running app to surface its flyout (used by a second launch that
    /// found an instance already running). A no-op on servers without a UI
    /// (e.g. `--headless`), which answer [`Response::Ok`] anyway.
    ShowFlyout,
}

impl Request {
    /// Validate the semantic invariants of a decoded request.
    ///
    /// # Errors
    /// [`IpcError::InvalidField`] if an id is out of charset/length or a
    /// percentage exceeds 100.
    pub fn validate(&self) -> Result<(), IpcError> {
        match self {
            Request::ListDisplays | Request::ShowFlyout => Ok(()),
            Request::GetBrightness { id } => validate_id(id),
            Request::SetBrightness { id, pct } => {
                validate_id(id)?;
                validate_pct(*pct)
            }
        }
    }
}

/// A reply from the app back to the client.
///
/// Internally tagged on `kind` and decoded leniently (unknown fields ignored)
/// for forward compatibility: `{"kind":"brightness","id":"…","pct":50}`,
/// `{"kind":"ok"}`, `{"kind":"error","code":"…","message":"…"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    /// The full display list, in the app's enumeration order.
    Displays {
        /// One entry per known display.
        displays: Vec<DisplayInfo>,
    },
    /// A single display's current unified brightness level.
    Brightness {
        /// The display this level belongs to.
        id: String,
        /// The current level, `0..=100`.
        pct: u8,
    },
    /// The command succeeded and carries no payload.
    Ok,
    /// The command failed; `code` is a stable machine token, `message` is human
    /// text.
    Error {
        /// A stable, lowercase error token (e.g. `"unknown_display"`).
        code: String,
        /// A human-readable explanation.
        message: String,
    },
}

impl Response {
    /// Validate the semantic invariants of a decoded response.
    ///
    /// # Errors
    /// [`IpcError::InvalidField`] if any id is out of charset/length or a
    /// percentage exceeds 100.
    pub fn validate(&self) -> Result<(), IpcError> {
        match self {
            Response::Ok | Response::Error { .. } => Ok(()),
            Response::Brightness { id, pct } => {
                validate_id(id)?;
                validate_pct(*pct)
            }
            Response::Displays { displays } => {
                for info in displays {
                    validate_id(&info.id)?;
                    validate_pct(info.level_pct)?;
                }
                Ok(())
            }
        }
    }
}

/// A display's public, UI-facing state, projected from a
/// [`DisplaySnapshot`] for transport.
///
/// `kind` and `features` are transport-local mirrors of the `duja_core` enums so
/// this crate can derive `serde` without imposing a serialization on the core
/// domain types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayInfo {
    /// The display's stable id (`[A-Za-z0-9#-]`, length `1..=64`).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// The display's physical class (external vs. built-in).
    pub kind: DisplayKindDto,
    /// Whether the display currently has no working hardware brightness and is
    /// dimmed purely in software — a runtime control-mode flag, independent of
    /// [`kind`](Self::kind).
    pub software_only: bool,
    /// The unified user brightness level, `0..=100`.
    pub level_pct: u8,
    /// The features the display reports as controllable, sorted.
    pub features: Vec<FeatureDto>,
}

impl DisplayInfo {
    /// Project a [`DisplaySnapshot`] into its wire form.
    #[must_use]
    pub fn from_snapshot(snapshot: &DisplaySnapshot) -> Self {
        DisplayInfo {
            id: snapshot.id.as_str().to_owned(),
            name: snapshot.name.clone(),
            kind: snapshot.kind.into(),
            software_only: snapshot.software_only,
            level_pct: snapshot.user_level_pct,
            features: snapshot
                .capabilities
                .features
                .iter()
                .map(|&f| f.into())
                .collect(),
        }
    }
}

/// Transport mirror of [`duja_core::model::DisplayKind`] — physical provenance
/// only. (Software-only is a separate flag, [`DisplayInfo::software_only`], not a
/// kind.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayKindDto {
    /// External monitor over DDC/CI.
    ExternalDdc,
    /// Built-in laptop/all-in-one panel.
    InternalPanel,
}

impl From<DisplayKind> for DisplayKindDto {
    fn from(kind: DisplayKind) -> Self {
        match kind {
            DisplayKind::ExternalDdc => DisplayKindDto::ExternalDdc,
            DisplayKind::InternalPanel => DisplayKindDto::InternalPanel,
        }
    }
}

impl From<DisplayKindDto> for DisplayKind {
    fn from(kind: DisplayKindDto) -> Self {
        match kind {
            DisplayKindDto::ExternalDdc => DisplayKind::ExternalDdc,
            DisplayKindDto::InternalPanel => DisplayKind::InternalPanel,
        }
    }
}

/// Transport mirror of [`duja_core::model::Feature`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureDto {
    /// Luminance / backlight.
    Brightness,
    /// Contrast.
    Contrast,
    /// Input source selection.
    InputSource,
}

impl From<Feature> for FeatureDto {
    fn from(feature: Feature) -> Self {
        match feature {
            Feature::Brightness => FeatureDto::Brightness,
            Feature::Contrast => FeatureDto::Contrast,
            Feature::InputSource => FeatureDto::InputSource,
        }
    }
}

impl From<FeatureDto> for Feature {
    fn from(feature: FeatureDto) -> Self {
        match feature {
            FeatureDto::Brightness => Feature::Brightness,
            FeatureDto::Contrast => Feature::Contrast,
            FeatureDto::InputSource => Feature::InputSource,
        }
    }
}

/// The strict wire envelope for a [`Request`]: version plus body, no extras.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestEnvelope {
    pub(crate) v: u16,
    pub(crate) request: Request,
}

/// The lenient wire envelope for a [`Response`]: version plus body, unknown
/// top-level keys ignored for forward compatibility.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ResponseEnvelope {
    pub(crate) v: u16,
    pub(crate) response: Response,
}

/// A minimal view used to read the version out of any envelope before trusting
/// the rest of the body. Unknown fields are ignored by design.
#[derive(Debug, Deserialize)]
pub(crate) struct VersionPeek {
    pub(crate) v: u16,
}

/// Reject a version this build cannot speak.
pub(crate) fn check_version(found: u16) -> Result<(), IpcError> {
    if found == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(IpcError::UnsupportedVersion {
            found,
            expected: PROTOCOL_VERSION,
        })
    }
}

/// Validate a display id: `1..=ID_MAX_LEN` characters from `[A-Za-z0-9#-]`.
fn validate_id(id: &str) -> Result<(), IpcError> {
    if id.is_empty() {
        return Err(IpcError::InvalidField {
            field: "id",
            reason: "must not be empty".to_owned(),
        });
    }
    if id.len() > ID_MAX_LEN {
        return Err(IpcError::InvalidField {
            field: "id",
            reason: format!("length {} exceeds {ID_MAX_LEN}", id.len()),
        });
    }
    if let Some(bad) = id
        .bytes()
        .find(|&b| !(b.is_ascii_alphanumeric() || b == b'-' || b == b'#'))
    {
        return Err(IpcError::InvalidField {
            field: "id",
            reason: format!("character {bad:#04x} is outside [A-Za-z0-9#-]"),
        });
    }
    Ok(())
}

/// Validate a brightness percentage: `0..=100`.
fn validate_pct(pct: u8) -> Result<(), IpcError> {
    if pct > 100 {
        Err(IpcError::InvalidField {
            field: "pct",
            reason: format!("{pct} exceeds 100"),
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_id_accepts_stable_id_shapes() {
        for id in [
            "GSM-5B09-312NTAB1C234",
            "DEL-A131-s12345",
            "DEL-A131-#h1a2b3c4d",
            "GSM-5B09-312NTAB1C234-slot2",
            "A",
        ] {
            assert!(validate_id(id).is_ok(), "rejected {id}");
        }
    }

    #[test]
    fn validate_id_rejects_empty_long_and_off_charset() {
        assert!(validate_id("").is_err());
        assert!(validate_id(&"a".repeat(ID_MAX_LEN + 1)).is_err());
        assert!(validate_id("has space").is_err());
        assert!(validate_id("under_score").is_err());
        assert!(validate_id("dot.dot").is_err());
        // Exactly the cap is fine.
        assert!(validate_id(&"a".repeat(ID_MAX_LEN)).is_ok());
    }

    #[test]
    fn validate_pct_bounds() {
        assert!(validate_pct(0).is_ok());
        assert!(validate_pct(100).is_ok());
        assert!(validate_pct(101).is_err());
        assert!(validate_pct(255).is_err());
    }

    #[test]
    fn request_validate_walks_variants() {
        assert!(Request::ListDisplays.validate().is_ok());
        assert!(
            Request::SetBrightness {
                id: "GSM-5B09-x".to_owned(),
                pct: 50
            }
            .validate()
            .is_ok()
        );
        assert!(
            Request::SetBrightness {
                id: "GSM-5B09-x".to_owned(),
                pct: 200
            }
            .validate()
            .is_err()
        );
        assert!(
            Request::GetBrightness {
                id: "bad id".to_owned()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn display_kind_dto_round_trips_both_physical_kinds() {
        // Exactly two variants — external and internal. Software-only is a flag, not
        // a kind, so it can never appear on the wire as a kind.
        for kind in [DisplayKind::ExternalDdc, DisplayKind::InternalPanel] {
            let dto: DisplayKindDto = kind.into();
            assert_eq!(DisplayKind::from(dto), kind);
        }
    }

    #[test]
    fn from_snapshot_carries_software_only_independently_of_kind() {
        use duja_core::id::StableDisplayId;
        use duja_core::model::{Capabilities, DisplaySnapshot};

        let make = |software_only: bool| DisplaySnapshot {
            id: StableDisplayId::from_parts("GSM", 0x5B09, Some("abc")).unwrap(),
            name: "Panel".to_owned(),
            kind: DisplayKind::InternalPanel,
            software_only,
            user_level_pct: 40,
            capabilities: Capabilities::default(),
        };
        // The flag rides through the projection while the kind stays InternalPanel.
        let sw = DisplayInfo::from_snapshot(&make(true));
        assert!(sw.software_only);
        assert_eq!(sw.kind, DisplayKindDto::InternalPanel);
        assert!(!DisplayInfo::from_snapshot(&make(false)).software_only);
    }
}
