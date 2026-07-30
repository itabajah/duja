//! A best-effort desktop notification announcing a newly-available update.
//!
//! Best-effort in the strongest sense: the tray "Update available" menu item and
//! the tooltip are the **guaranteed** surfaces, and this is a bonus on top. Every
//! failure is logged at WARN and swallowed, and a platform with no implementation
//! is not a failure at all — it simply has no bonus.
//!
//! Compiled only where the tray is, since `tray::update_flow` is its one caller.
//!
//! # Windows
//!
//! A `WinRT` `ToastNotification` through the `windows` crate already in the build,
//! so no extra dependency. An unpackaged process must set an explicit
//! `AppUserModelID` for a toast to resolve an identity: `AUMID` is set on the
//! process, and the installer stamps the *same* id on the Start-Menu shortcut,
//! which is what makes the toast render reliably for an installed copy. A portable
//! (unzipped) copy has no shortcut, so its toast may show a generic identity or be
//! suppressed — acceptable, and documented, since the tray surfaces cover it.
//!
//! The toast's `launch` opens the releases page via protocol activation, so a
//! click behaves like the tray item; Duja still only ever opens the page.
//!
//! # macOS — deliberately nothing, for now
//!
//! There is no macOS arm, and it is not an oversight to fill in later without
//! thought. `UNUserNotificationCenter` — the only supported route since
//! `NSUserNotification` was removed — requires **a signed application bundle**,
//! which Duja does not have until the packaging work lands, *and* a runtime
//! authorization prompt, which is a product decision: asking a brightness utility
//! for notification permission on first launch, to deliver one message a user may
//! never see, is a worse trade than the tray item they already have.
//!
//! So on macOS the update surfaces through the tray menu item and tooltip only.
//! That is a complete path, not a degraded one — see `update_flow`, where the menu
//! item and tooltip are set *before* this is called and independently of it.

#[cfg(windows)]
use windows::Data::Xml::Dom::XmlDocument;
#[cfg(windows)]
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
#[cfg(windows)]
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
#[cfg(windows)]
use windows::core::{HSTRING, PCWSTR};

#[cfg(windows)]
use crate::bin_support::updates::RELEASES_PAGE_URL;

/// The application's stable `AppUserModelID`. Must match the `AppUserModelID` the
/// installer stamps on the Start-Menu shortcut (`packaging/windows/duja.iss`).
#[cfg(windows)]
const AUMID: &str = "io.github.itabajah.duja";

/// Announce that `version` is available, where the platform supports it.
///
/// Best-effort and infallible by construction: a platform failure is logged, and
/// a platform with no notification path does nothing at all. Callers must not
/// treat this as the update's delivery mechanism — the tray item is.
pub(crate) fn notify_update_available(version: &str) {
    platform::notify(version);
}

#[cfg(windows)]
mod platform {
    use tracing::warn;

    /// Show the toast, logging any `WinRT` failure.
    pub(super) fn notify(version: &str) {
        if let Err(e) = super::show(version) {
            warn!(error = %e, "failed to show the update toast");
        }
    }
}

#[cfg(not(windows))]
mod platform {
    /// No notification path on this platform (see the module docs). Silent by
    /// design: there is nothing that failed, so there is nothing to warn about,
    /// and the tray item has already surfaced the update by the time this runs.
    pub(super) const fn notify(_version: &str) {}
}

/// Build and show the toast, propagating any `WinRT` error to the caller for
/// logging.
#[cfg(windows)]
fn show(version: &str) -> windows::core::Result<()> {
    set_app_id()?;

    let body = format!(
        "Version {} is available. Open the releases page to download.",
        xml_escape(version)
    );
    let xml = format!(
        "<toast activationType=\"protocol\" launch=\"{launch}\">\
           <visual>\
             <binding template=\"ToastGeneric\">\
               <text>Duja update available</text>\
               <text>{body}</text>\
             </binding>\
           </visual>\
         </toast>",
        launch = xml_escape(RELEASES_PAGE_URL),
    );

    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?;
    notifier.Show(&toast)
}

/// Set the process `AppUserModelID` so the toast has an app identity.
#[cfg(windows)]
fn set_app_id() -> windows::core::Result<()> {
    let wide: Vec<u16> = AUMID.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call;
    // the function only reads it. Setting the id is idempotent.
    unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(wide.as_ptr())) }
}

/// Escape the five XML metacharacters so a version/URL can be embedded in the
/// toast payload safely.
///
/// Only the Windows toast builds XML, but this stays compiled under `test` so the
/// escaping table is pinned wherever this module is.
///
/// That is **two** lanes, not three: gating `mod toast` where its one caller lives
/// took Linux out of the promise an earlier version of this comment made. Low risk
/// — the table is only *used* on Windows, which does run the test — but worth
/// stating rather than leaving a stale claim of wider coverage. The `test` half of
/// the `cfg` therefore only ever adds this function on macOS.
#[cfg(any(test, windows))]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::xml_escape;

    #[test]
    fn xml_escape_covers_the_metacharacters() {
        assert_eq!(
            xml_escape("v1.0 <a> & \"b\" 'c'"),
            "v1.0 &lt;a&gt; &amp; &quot;b&quot; &apos;c&apos;"
        );
        // A normal semver tag is untouched.
        assert_eq!(xml_escape("v0.1.0"), "v0.1.0");
    }
}
