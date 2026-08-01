//! The pure half of the macOS display ↔ I2C-service pairing rule.
//!
//! Apple exposes no direct `CGDirectDisplayID` → `IOAVService` (or
//! `IOFramebuffer`) link, so [`mac::sys`](crate::mac) pairs external displays to
//! external services **positionally**. That policy is a two-line decision with a
//! sharp failure mode — "the slider moves the wrong monitor" — and it used to
//! live inside a `cfg(target_os = "macos")` FFI loop where no lane could reach
//! it. It lives here instead: FFI-free, so it is compiled and its tests run on
//! **every** target, exactly like [`correlate`](crate::correlate) does for the
//! Windows path.
//!
//! # The rule
//!
//! Display *i* pairs with service *i*, and **nothing downstream may change
//! that**. The distinction that matters is what happens to a display the caller
//! then discards — one whose EDID cannot be read, say. Its slot is still spent:
//! the service at that index belongs to it and to no one else, so display *i+1*
//! must still get service *i+1*.
//!
//! Getting that wrong is not a rounding error, it is an off-by-one that
//! **silently re-points every display after the skipped one**. The bug this
//! module was extracted to prevent read as a queue — pop a service per surviving
//! display — with the EDID check *above* the pop, so one unreadable EDID handed
//! monitor #2 monitor #1's service for the rest of the session. On a two-monitor
//! Mac where the first display is a `DisplayLink` or dock panel with no
//! `IODisplayEDIDOriginal`, dragging the second monitor's slider moved the first
//! one's brightness.
//!
//! [`pair_positionally`] removes the possibility rather than fixing the instance:
//! the assignment is made **before** the caller can filter anything, so a later
//! `continue` cannot desynchronise a queue that no longer exists.

/// Pair each display with the service at its own index, in order.
///
/// Every display appears in the result, in its original order, carrying `Some`
/// service when one exists at that index and `None` once the services run out
/// (fewer services than displays is ordinary — a display Duja cannot drive over
/// I2C at all). The caller discards the `None`s *after* pairing; discarding
/// before is the desynchronisation described in the [module docs](self).
///
/// Surplus services are dropped, which releases them: on macOS both service
/// types own an `IOKit` handle whose `Drop` closes it.
pub(crate) fn pair_positionally<D, S>(displays: Vec<D>, services: Vec<S>) -> Vec<(D, Option<S>)> {
    let mut services = services.into_iter();
    displays
        .into_iter()
        .map(|display| (display, services.next()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_display_takes_the_service_at_its_own_index() {
        let paired = pair_positionally(vec!['a', 'b', 'c'], vec![10, 20, 30]);
        assert_eq!(
            paired,
            vec![('a', Some(10)), ('b', Some(20)), ('c', Some(30))]
        );
    }

    #[test]
    fn a_display_the_caller_will_discard_still_spends_its_slot() {
        // The regression this module exists for. Display 'b' has no readable
        // EDID, so the caller drops it — but 'c' must still get service 30, not
        // 'b's 20. Pairing first is what makes that true: the caller cannot
        // express the wrong answer, because it filters a list that is already
        // assigned.
        let paired = pair_positionally(vec!['a', 'b', 'c'], vec![10, 20, 30]);
        let kept: Vec<_> = paired.into_iter().filter(|(d, _)| *d != 'b').collect();
        assert_eq!(
            kept,
            vec![('a', Some(10)), ('c', Some(30))],
            "discarding a display must not re-point the ones after it"
        );
    }

    #[test]
    fn displays_beyond_the_services_pair_with_nothing() {
        let paired = pair_positionally(vec!['a', 'b', 'c'], vec![10]);
        assert_eq!(paired, vec![('a', Some(10)), ('b', None), ('c', None)]);
    }

    #[test]
    fn surplus_services_are_dropped_rather_than_reassigned() {
        // Two displays, three services: the third is not handed to anyone, and
        // in particular is not given to the first display as a fallback.
        let paired = pair_positionally(vec!['a', 'b'], vec![10, 20, 30]);
        assert_eq!(paired, vec![('a', Some(10)), ('b', Some(20))]);
    }

    #[test]
    fn no_displays_yields_no_pairs_even_with_services_available() {
        let paired: Vec<(char, Option<i32>)> = pair_positionally(Vec::new(), vec![10, 20]);
        assert!(paired.is_empty());
    }

    #[test]
    fn no_services_still_reports_every_display() {
        // A Mac with no reachable I2C service must not silently lose displays
        // here — the caller decides what a bus-less display means.
        let paired: Vec<(char, Option<i32>)> = pair_positionally(vec!['a', 'b'], Vec::new());
        assert_eq!(paired, vec![('a', None), ('b', None)]);
    }
}
