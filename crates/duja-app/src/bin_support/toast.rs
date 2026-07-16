//! A best-effort Windows toast announcing a newly-available update.
//!
//! This uses the `WinRT` `ToastNotification` API through the `windows` crate
//! already in the build — no extra dependency. Every call is best-effort: a
//! failure is logged at WARN and swallowed (exactly like `tray::open_url`),
//! because a missing toast must never affect the app. The tray "Update
//! available" item and tooltip are the guaranteed surfaces; the toast is a
//! bonus.
//!
//! # App identity
//!
//! An unpackaged process must set an explicit `AppUserModelID` for a toast to
//! resolve an identity. We set [`AUMID`] on the process; the installer sets the
//! *same* id on the Start-Menu shortcut, which is what makes the toast render
//! reliably for an installed copy. A portable (unzipped) copy has no shortcut,
//! so its toast may show a generic identity or be suppressed — acceptable, and
//! documented, since the tray surfaces cover it.
//!
//! The toast's `launch` opens the releases page via protocol activation, so a
//! click behaves like the tray item; Duja still only ever opens the page.

use tracing::warn;
use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use windows::core::{HSTRING, PCWSTR};

use crate::bin_support::updates::RELEASES_PAGE_URL;

/// The application's stable `AppUserModelID`. Must match the `AppUserModelID` the
/// installer stamps on the Start-Menu shortcut (`packaging/windows/duja.iss`).
const AUMID: &str = "io.github.itabajah.duja";

/// Show a toast announcing that `version` is available. Best-effort — logs and
/// returns on any failure.
pub(crate) fn notify_update_available(version: &str) {
    if let Err(e) = show(version) {
        warn!(error = %e, "failed to show the update toast");
    }
}

/// Build and show the toast, propagating any `WinRT` error to the caller for
/// logging.
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
fn set_app_id() -> windows::core::Result<()> {
    let wide: Vec<u16> = AUMID.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call;
    // the function only reads it. Setting the id is idempotent.
    unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(wide.as_ptr())) }
}

/// Escape the five XML metacharacters so a version/URL can be embedded in the
/// toast payload safely.
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
