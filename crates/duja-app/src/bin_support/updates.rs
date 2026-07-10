//! The opt-in update check (plan §6.3): one HTTPS GET, a semver compare, and a
//! browser hand-off — Duja never downloads or installs anything itself.
//!
//! # Off by default, manual only
//!
//! No network request happens unless the user turns the check on
//! (`general.update_check`) **and** triggers it — from the settings window or
//! the `duja --check-updates` CLI. There is **no** background timer (the
//! zero-idle-wakeup rule; scheduled checks are a post-1.0 item), so this module
//! runs only on an explicit user action.
//!
//! # Shape
//!
//! The decision logic is pure: [`check_for_update`] takes an injected
//! [`UpdateTransport`] and the compiled-in version and returns an
//! [`UpdateOutcome`]. That makes every branch — newer / older / equal /
//! pre-release / garbage JSON / oversized / network error — unit-testable
//! against a fake transport, with no socket in sight.
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
    /// The running build is the newest stable release (or the latest release is
    /// a pre-release, which never prompts).
    UpToDate,
    /// A strictly-newer stable release is available; carries its tag as printed
    /// by GitHub (e.g. `v1.2.0`).
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
/// [`UpdateOutcome::Failed`]; a latest release that is a pre-release or not
/// strictly newer yields [`UpdateOutcome::UpToDate`] (conservative — only a
/// strictly-greater *stable* version prompts).
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
        // A newer STABLE release: prompt.
        Some(latest) if latest > current_v => UpdateOutcome::UpdateAvailable { version: tag },
        // Equal, older, a pre-release, or an unparseable tag: never prompt.
        _ => UpdateOutcome::UpToDate,
    }
}

/// Extract the `tag_name` string from a GitHub release JSON body.
fn parse_tag_name(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value.get("tag_name")?.as_str().map(str::to_owned)
}

/// A parsed *stable* semantic version: `major.minor.patch` with no pre-release
/// or build metadata.
///
/// The field order makes the derived [`Ord`] compare major, then minor, then
/// patch — exactly semver's precedence for release versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    /// Parse a `[v]MAJOR.MINOR.PATCH` tag, returning `None` for anything with a
    /// pre-release (`-rc1`) or build (`+meta`) suffix, extra components, or a
    /// non-numeric field.
    ///
    /// Pre-releases deliberately do not parse: only a strictly-greater stable
    /// release should ever prompt the user (plan §6.3).
    fn parse(tag: &str) -> Option<Version> {
        let trimmed = tag.trim();
        let body = trimmed
            .strip_prefix('v')
            .or_else(|| trimmed.strip_prefix('V'))
            .unwrap_or(trimmed);
        // A stable release has no pre-release or build metadata.
        if body.contains('-') || body.contains('+') {
            return None;
        }
        let mut parts = body.split('.');
        let major = parts.next()?.parse::<u64>().ok()?;
        let minor = parts.next()?.parse::<u64>().ok()?;
        let patch = parts.next()?.parse::<u64>().ok()?;
        if parts.next().is_some() {
            return None; // more than three dotted components
        }
        Some(Version {
            major,
            minor,
            patch,
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
    fn newer_prerelease_never_prompts() {
        // A pre-release is conservatively ignored even if numerically higher.
        let t = FakeTransport::body(&release("v2.0.0-rc1"));
        assert_eq!(check_for_update(&t, "1.0.0"), UpdateOutcome::UpToDate);
        let t = FakeTransport::body(&release("v2.0.0+build.7"));
        assert_eq!(check_for_update(&t, "1.0.0"), UpdateOutcome::UpToDate);
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
    fn version_parse_rejects_prerelease_and_junk() {
        assert_eq!(
            Version::parse("v1.2.3"),
            Some(Version {
                major: 1,
                minor: 2,
                patch: 3
            })
        );
        assert_eq!(Version::parse("1.2.3"), Version::parse("v1.2.3"));
        assert_eq!(Version::parse("1.2.3-rc1"), None);
        assert_eq!(Version::parse("1.2"), None);
        assert_eq!(Version::parse("1.2.3.4"), None);
        assert_eq!(Version::parse("banana"), None);
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
