//! The update check: one HTTPS GET, a semver compare, and a browser hand-off —
//! Duja never downloads or installs anything itself.
//!
//! # On by default, notify-only
//!
//! The check runs while `general.update_check` is on (the default). It is not a
//! timer: a once-a-day background check piggybacks on events the process is
//! already handling (tray interaction, startup), so the zero-idle-wakeup
//! guarantee is untouched (see `tray::maybe_background_update_check`). The
//! manual paths — the settings window "Check now" button and the
//! `duja --check-updates` CLI — still work regardless of the toggle. On a newer
//! release Duja surfaces it (tray item, tooltip, toast) and, when acted on,
//! opens the releases page in the browser; it **never** downloads or installs.
//!
//! # Shape
//!
//! The decision logic is pure: [`check_for_update`] takes an injected
//! [`UpdateTransport`] and the compiled-in version and returns an
//! [`UpdateOutcome`]. That makes every branch — newer / older / equal /
//! pre-release ordering / garbage JSON / oversized / network error —
//! unit-testable against a fake transport, with no socket in sight.
//!
//! The real transport ([`HttpsTransport`]) wraps `ureq` (rustls) with 5-second
//! timeouts and reads the body **read-limited** to [`MAX_RESPONSE_BYTES`] before
//! buffering, so a hostile or broken endpoint cannot make Duja allocate without
//! bound. Its live smoke test is `#[ignore]`d behind `DUJA_NET_TESTS=1`.

use std::io::Read;

use tracing::warn;

/// The GitHub "latest release" API endpoint Duja queries.
pub const RELEASES_API_URL: &str = "https://api.github.com/repos/itabajah/duja/releases/latest";

/// The human-facing releases page opened in the browser on an available update.
pub const RELEASES_PAGE_URL: &str = "https://github.com/itabajah/duja/releases/latest";

/// The hard cap on the response body Duja will buffer (64 KiB): the releases
/// JSON is a few KiB, so this is generous while still bounding memory against a
/// misbehaving endpoint.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// The per-operation network timeout for the real transport.
const NETWORK_TIMEOUT_SECS: u64 = 5;

/// A one-shot HTTPS fetch, injected so the decision logic can be tested without
/// a socket.
pub trait UpdateTransport {
    /// GET `url` and return the (already length-capped) response body.
    ///
    /// # Errors
    /// [`TransportError`] carrying a human-readable reason on any network, TLS,
    /// or HTTP-status failure.
    fn fetch(&self, url: &str) -> Result<Vec<u8>, TransportError>;
}

/// A transport failure, carrying a reason string for the WARN log.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct TransportError(pub String);

/// The result of an update check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// The running build is the newest release (nothing strictly newer, or the
    /// latest tag did not parse as a version).
    UpToDate,
    /// A strictly-newer release is available; carries its tag as printed by
    /// GitHub (e.g. `v1.2.0`).
    UpdateAvailable {
        /// The newer release's tag name.
        version: String,
    },
    /// The check could not be completed. The reason is logged at WARN; the UI
    /// shows a neutral "couldn't check" line rather than this string.
    Failed(String),
}

/// Run the update check against `transport`, comparing the latest release to
/// `current_version` (normally `env!("CARGO_PKG_VERSION")`).
///
/// Pure with respect to the transport: no I/O of its own, so it is exhaustively
/// unit-testable. A failed fetch or an unparseable response yields
/// [`UpdateOutcome::Failed`]; a latest release that is not strictly newer than
/// the running build yields [`UpdateOutcome::UpToDate`]. Only a
/// strictly-greater version (by `SemVer` precedence) prompts.
#[must_use]
pub fn check_for_update(transport: &dyn UpdateTransport, current_version: &str) -> UpdateOutcome {
    match transport.fetch(RELEASES_API_URL) {
        Ok(body) => evaluate(&body, current_version),
        Err(TransportError(reason)) => {
            warn!(reason = %reason, "update check failed");
            UpdateOutcome::Failed(reason)
        }
    }
}

/// Decide the outcome from a raw response body and the current version.
fn evaluate(body: &[u8], current: &str) -> UpdateOutcome {
    let Some(tag) = parse_tag_name(body) else {
        let reason = "release response had no parseable `tag_name`".to_owned();
        warn!(reason, "update check failed");
        return UpdateOutcome::Failed(reason);
    };
    let Some(current_v) = Version::parse(current) else {
        // Should never happen for our own compiled-in version, but stay honest.
        let reason = format!("could not parse the running version `{current}`");
        warn!(reason, "update check failed");
        return UpdateOutcome::Failed(reason);
    };
    match Version::parse(&tag) {
        // Strictly newer by `SemVer` precedence: prompt.
        Some(latest) if latest > current_v => UpdateOutcome::UpdateAvailable { version: tag },
        // Equal, older, or an unparseable tag: do not prompt.
        _ => UpdateOutcome::UpToDate,
    }
}

/// Extract the `tag_name` string from a GitHub release JSON body.
fn parse_tag_name(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value.get("tag_name")?.as_str().map(str::to_owned)
}

/// One dot-separated identifier of a `SemVer` pre-release string.
///
/// Per `SemVer` §11, an all-numeric identifier compares numerically and ranks
/// below an alphanumeric one; two alphanumerics compare in ASCII order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PreId {
    /// An all-ASCII-digit identifier (e.g. `1`), compared numerically.
    Numeric(u64),
    /// Any other identifier (e.g. `rc`, `alpha`), compared as ASCII text.
    Alnum(String),
}

impl Ord for PreId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self, other) {
            (PreId::Numeric(a), PreId::Numeric(b)) => a.cmp(b),
            (PreId::Alnum(a), PreId::Alnum(b)) => a.cmp(b),
            // Numeric identifiers always have lower precedence than alphanumeric.
            (PreId::Numeric(_), PreId::Alnum(_)) => Ordering::Less,
            (PreId::Alnum(_), PreId::Numeric(_)) => Ordering::Greater,
        }
    }
}

impl PartialOrd for PreId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A parsed semantic version: `major.minor.patch` with optional pre-release
/// identifiers. Build metadata is parsed away and ignored (`SemVer` §10).
///
/// [`Ord`] implements `SemVer` §11 precedence: compare the numeric core, then —
/// on a tie — a version *with* a pre-release ranks below one without, and
/// otherwise the pre-release identifier lists compare element-wise (a longer
/// list winning when every shared identifier is equal).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    /// Empty for a stable release; the ordered identifiers otherwise.
    pre: Vec<PreId>,
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch)) {
            Ordering::Equal => match (self.pre.is_empty(), other.pre.is_empty()) {
                // A stable release outranks any pre-release of the same core.
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                // Both pre-releases: compare identifier lists element-wise; if
                // all shared identifiers tie, the longer list has higher
                // precedence (`Iterator::cmp` already gives this).
                (false, false) => self.pre.iter().cmp(other.pre.iter()),
            },
            core => core,
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Version {
    /// Parse a `[v]MAJOR.MINOR.PATCH[-pre][+build]` tag.
    ///
    /// Returns `None` for a non-numeric or missing core field, more than three
    /// core components, or an empty pre-release identifier. Build metadata
    /// (`+…`) is accepted and discarded — it does not affect precedence.
    fn parse(tag: &str) -> Option<Version> {
        let trimmed = tag.trim();
        let body = trimmed
            .strip_prefix('v')
            .or_else(|| trimmed.strip_prefix('V'))
            .unwrap_or(trimmed);
        // Strip build metadata first (`SemVer` §10: ignored in precedence).
        let body = body.split('+').next().unwrap_or(body);
        // Split off the pre-release at the first '-'.
        let (core, pre) = match body.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (body, None),
        };

        let mut parts = core.split('.');
        let major = parts.next()?.parse::<u64>().ok()?;
        let minor = parts.next()?.parse::<u64>().ok()?;
        let patch = parts.next()?.parse::<u64>().ok()?;
        if parts.next().is_some() {
            return None; // more than three dotted core components
        }

        let pre = match pre {
            None => Vec::new(),
            Some(pre) => {
                let mut ids = Vec::new();
                for id in pre.split('.') {
                    if id.is_empty() {
                        return None; // empty identifier, e.g. `1.0.0-` or `1.0.0-a..b`
                    }
                    // An identifier that is all ASCII digits compares numerically.
                    if id.bytes().all(|b| b.is_ascii_digit()) {
                        ids.push(PreId::Numeric(id.parse::<u64>().ok()?));
                    } else {
                        ids.push(PreId::Alnum(id.to_owned()));
                    }
                }
                ids
            }
        };

        Some(Version {
            major,
            minor,
            patch,
            pre,
        })
    }
}

/// Read at most `max` bytes from `reader`, returning what was read.
///
/// The cap is applied by limiting the reader (`take`) so a body larger than
/// `max` is truncated **before** it is fully buffered — a hostile endpoint
/// cannot force an unbounded allocation. Pure over any [`Read`], so the cap is
/// unit-tested without a network.
///
/// # Errors
/// Propagates any read error from the underlying reader.
fn read_capped(reader: impl Read, max: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    reader.take(max as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

/// The real HTTPS transport: `ureq` (rustls) with fixed short timeouts and a
/// read-limited body.
///
/// Only linked into the update-check paths (the CLI flag and the Windows tray
/// action). Its one live test hits the network and is `#[ignore]`d.
pub struct HttpsTransport;

impl UpdateTransport for HttpsTransport {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, TransportError> {
        use std::time::Duration;

        let timeout = Duration::from_secs(NETWORK_TIMEOUT_SECS);
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(timeout)
            .timeout_read(timeout)
            .timeout_write(timeout)
            .build();
        let response = agent
            .get(url)
            // GitHub's API rejects requests without a User-Agent.
            .set("User-Agent", concat!("duja/", env!("CARGO_PKG_VERSION")))
            .set("Accept", "application/vnd.github+json")
            .call()
            .map_err(|e| TransportError(e.to_string()))?;
        read_capped(response.into_reader(), MAX_RESPONSE_BYTES)
            .map_err(|e| TransportError(format!("reading response body: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A transport that returns a canned body (or a canned error).
    struct FakeTransport(Result<Vec<u8>, TransportError>);

    impl FakeTransport {
        fn body(json: &str) -> Self {
            FakeTransport(Ok(json.as_bytes().to_vec()))
        }
        fn raw(bytes: Vec<u8>) -> Self {
            FakeTransport(Ok(bytes))
        }
        fn error(reason: &str) -> Self {
            FakeTransport(Err(TransportError(reason.to_owned())))
        }
    }

    impl UpdateTransport for FakeTransport {
        fn fetch(&self, _url: &str) -> Result<Vec<u8>, TransportError> {
            self.0.clone()
        }
    }

    fn release(tag: &str) -> String {
        format!("{{\"tag_name\": \"{tag}\", \"name\": \"Release {tag}\"}}")
    }

    #[test]
    fn newer_stable_release_prompts() {
        let t = FakeTransport::body(&release("v1.2.0"));
        assert_eq!(
            check_for_update(&t, "1.0.0"),
            UpdateOutcome::UpdateAvailable {
                version: "v1.2.0".to_owned()
            }
        );
    }

    #[test]
    fn older_release_is_up_to_date() {
        let t = FakeTransport::body(&release("v0.9.0"));
        assert_eq!(check_for_update(&t, "1.0.0"), UpdateOutcome::UpToDate);
    }

    #[test]
    fn version_components_compare_numerically_not_lexically() {
        // The classic trap: "1.10.0" < "1.9.0" as strings, but 10 > 9 as
        // numbers. Both directions, on minor and patch.
        let t = FakeTransport::body(&release("v1.10.0"));
        assert_eq!(
            check_for_update(&t, "1.9.0"),
            UpdateOutcome::UpdateAvailable {
                version: "v1.10.0".to_owned()
            }
        );
        let t = FakeTransport::body(&release("v1.9.0"));
        assert_eq!(check_for_update(&t, "1.10.0"), UpdateOutcome::UpToDate);

        let t = FakeTransport::body(&release("v1.0.10"));
        assert_eq!(
            check_for_update(&t, "1.0.9"),
            UpdateOutcome::UpdateAvailable {
                version: "v1.0.10".to_owned()
            }
        );
        let t = FakeTransport::body(&release("v1.0.9"));
        assert_eq!(check_for_update(&t, "1.0.10"), UpdateOutcome::UpToDate);
    }

    #[test]
    fn equal_release_is_up_to_date() {
        let t = FakeTransport::body(&release("v1.0.0"));
        assert_eq!(check_for_update(&t, "1.0.0"), UpdateOutcome::UpToDate);
        // The `v` prefix is optional on either side.
        let t = FakeTransport::body(&release("1.0.0"));
        assert_eq!(check_for_update(&t, "1.0.0"), UpdateOutcome::UpToDate);
    }

    #[test]
    fn newer_prerelease_prompts() {
        // A newer pre-release (higher core) still prompts under `SemVer` ordering.
        // In practice GitHub's `/releases/latest` never returns a pre-release,
        // but the comparison must be correct for the alpha/beta line.
        let t = FakeTransport::body(&release("v2.0.0-rc1"));
        assert_eq!(
            check_for_update(&t, "1.0.0"),
            UpdateOutcome::UpdateAvailable {
                version: "v2.0.0-rc1".to_owned()
            }
        );
    }

    #[test]
    fn build_metadata_is_ignored() {
        // `+build` does not affect precedence: same core ⇒ up to date.
        let t = FakeTransport::body(&release("v1.0.0+build.7"));
        assert_eq!(check_for_update(&t, "1.0.0"), UpdateOutcome::UpToDate);
        assert_eq!(Version::parse("1.0.0+build.7"), Version::parse("1.0.0"));
    }

    #[test]
    fn a_prerelease_is_lower_than_its_stable_release() {
        // 2.0.0-rc.1 < 2.0.0, so a running rc is offered the stable release.
        let t = FakeTransport::body(&release("v2.0.0"));
        assert_eq!(
            check_for_update(&t, "2.0.0-rc.1"),
            UpdateOutcome::UpdateAvailable {
                version: "v2.0.0".to_owned()
            }
        );
        // …but the same rc is not offered its own pre-release.
        let t = FakeTransport::body(&release("v2.0.0-rc.1"));
        assert_eq!(check_for_update(&t, "2.0.0-rc.1"), UpdateOutcome::UpToDate);
    }

    #[test]
    fn semver_prerelease_precedence_chain() {
        // `SemVer` §11 worked example, in strictly increasing order.
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ];
        for pair in chain.windows(2) {
            let [lower, higher] = pair else { continue };
            let lo = Version::parse(lower).expect("parses");
            let hi = Version::parse(higher).expect("parses");
            assert!(lo < hi, "{lower} should be < {higher}");
            assert!(hi > lo, "{higher} should be > {lower}");
        }
    }

    #[test]
    fn garbage_json_fails() {
        let t = FakeTransport::body("this is not json");
        assert!(matches!(
            check_for_update(&t, "1.0.0"),
            UpdateOutcome::Failed(_)
        ));
        // Valid JSON but no tag_name also fails.
        let t = FakeTransport::body("{\"name\": \"x\"}");
        assert!(matches!(
            check_for_update(&t, "1.0.0"),
            UpdateOutcome::Failed(_)
        ));
    }

    #[test]
    fn unparseable_tag_does_not_prompt() {
        let t = FakeTransport::body(&release("nightly"));
        assert_eq!(check_for_update(&t, "1.0.0"), UpdateOutcome::UpToDate);
    }

    #[test]
    fn network_error_is_failed() {
        let t = FakeTransport::error("connection refused");
        assert_eq!(
            check_for_update(&t, "1.0.0"),
            UpdateOutcome::Failed("connection refused".to_owned())
        );
    }

    #[test]
    fn oversized_truncated_body_fails_gracefully() {
        // Simulate what the read-limited transport hands back for a huge body:
        // a 64 KiB blob that is not valid JSON. It must not panic; it Fails.
        let big = vec![b'{'; MAX_RESPONSE_BYTES];
        let t = FakeTransport::raw(big);
        assert!(matches!(
            check_for_update(&t, "1.0.0"),
            UpdateOutcome::Failed(_)
        ));
    }

    #[test]
    fn read_capped_truncates_before_buffering() {
        let data = vec![b'x'; MAX_RESPONSE_BYTES * 2];
        let out = read_capped(&data[..], MAX_RESPONSE_BYTES).expect("read");
        assert_eq!(out.len(), MAX_RESPONSE_BYTES);
    }

    #[test]
    fn read_capped_keeps_a_short_body_whole() {
        let data = b"short body".to_vec();
        let out = read_capped(&data[..], MAX_RESPONSE_BYTES).expect("read");
        assert_eq!(out, data);
    }

    #[test]
    fn version_parse_accepts_core_and_prerelease_rejects_junk() {
        assert_eq!(
            Version::parse("v1.2.3"),
            Some(Version {
                major: 1,
                minor: 2,
                patch: 3,
                pre: Vec::new(),
            })
        );
        assert_eq!(Version::parse("1.2.3"), Version::parse("v1.2.3"));
        // Pre-release identifiers now parse and round-trip (numeric vs alnum).
        assert_eq!(
            Version::parse("1.2.3-rc.2"),
            Some(Version {
                major: 1,
                minor: 2,
                patch: 3,
                pre: vec![PreId::Alnum("rc".to_owned()), PreId::Numeric(2)],
            })
        );
        // Junk and malformed cores still reject.
        assert_eq!(Version::parse("1.2"), None);
        assert_eq!(Version::parse("1.2.3.4"), None);
        assert_eq!(Version::parse("banana"), None);
        assert_eq!(Version::parse("1.2.3-"), None); // empty pre-release
        assert_eq!(Version::parse("1.2.3-a..b"), None); // empty identifier
    }

    // A live smoke test against the real GitHub API. Never run in CI or the
    // disconnected session: it is `#[ignore]`d and further gated on
    // `DUJA_NET_TESTS=1` so `--ignored` alone will not fire it.
    #[test]
    #[ignore = "hits the network; run with DUJA_NET_TESTS=1 and --ignored"]
    fn live_github_fetch_smoke() {
        if std::env::var("DUJA_NET_TESTS").as_deref() != Ok("1") {
            return;
        }
        let outcome = check_for_update(&HttpsTransport, env!("CARGO_PKG_VERSION"));
        // We only assert it did not panic and produced a decision.
        assert!(matches!(
            outcome,
            UpdateOutcome::UpToDate
                | UpdateOutcome::UpdateAvailable { .. }
                | UpdateOutcome::Failed(_)
        ));
    }
}
