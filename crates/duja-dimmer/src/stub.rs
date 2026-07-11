//! The [`Dimmer`] stub for targets without a native overlay backend.
//!
//! Windows and macOS have real backends; Linux (P7) gets one later. Until then
//! this records-and-succeeds: [`apply`](Dimmer::apply) and
//! [`clear`](Dimmer::clear) validate and remember the request but touch no
//! screen, so the app's control logic, tests, and the diffing kernel run
//! unchanged on every target. It is a documented no-op, **not** an error —
//! higher layers treat "dimming applied" uniformly and the missing pixels are a
//! platform limitation, not a fault.

use duja_core::dimmer::{DimCommand, Dimmer, DimmerError};

use crate::plan::{OverlayEntry, apply_ops, plan_transition};

/// A recording, no-op [`Dimmer`] for platforms without a native overlay backend.
///
/// It keeps the same *current-overlay* bookkeeping a real backend would (via the
/// pure [`plan`](crate::plan) kernel), so [`current`](Self::current) reflects
/// exactly what the Windows backend would be showing — only the OS windows are
/// absent.
#[derive(Debug, Default)]
pub struct StubDimmer {
    current: Vec<OverlayEntry>,
    applies: usize,
    clears: usize,
}

impl StubDimmer {
    /// A fresh stub with no recorded state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The overlays a real backend would be showing after the last apply.
    #[must_use]
    pub fn current(&self) -> &[OverlayEntry] {
        &self.current
    }

    /// How many [`apply`](Dimmer::apply) calls have been recorded.
    #[must_use]
    pub fn apply_count(&self) -> usize {
        self.applies
    }

    /// How many [`clear`](Dimmer::clear) calls have been recorded.
    #[must_use]
    pub fn clear_count(&self) -> usize {
        self.clears
    }
}

impl Dimmer for StubDimmer {
    fn apply(&mut self, commands: &[DimCommand]) -> Result<(), DimmerError> {
        let ops = plan_transition(&self.current, commands);
        self.current = apply_ops(&self.current, &ops);
        self.applies = self.applies.saturating_add(1);
        Ok(())
    }

    fn clear(&mut self) -> Result<(), DimmerError> {
        self.current.clear();
        self.clears = self.clears.saturating_add(1);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duja_core::dimmer::DisplayBounds;
    use duja_core::id::StableDisplayId;

    fn cmd(serial: &str, alpha: f32) -> DimCommand {
        let id = StableDisplayId::from_parts("AAA", 0x0001, Some(serial)).unwrap();
        DimCommand::new(id, DisplayBounds::new(0, 0, 100, 100), alpha, None)
    }

    #[test]
    fn tracks_current_like_a_real_backend() {
        let mut d = StubDimmer::new();
        d.apply(&[cmd("a", 0.5), cmd("b", 0.0)]).unwrap();
        assert_eq!(d.current().len(), 1);
        assert_eq!(d.apply_count(), 1);
    }

    #[test]
    fn clear_empties_and_counts() {
        let mut d = StubDimmer::new();
        d.apply(&[cmd("a", 0.5)]).unwrap();
        d.clear().unwrap();
        assert!(d.current().is_empty());
        assert_eq!(d.clear_count(), 1);
    }

    #[test]
    fn is_a_dimmer_trait_object() {
        let mut d: Box<dyn Dimmer> = Box::new(StubDimmer::new());
        assert!(d.apply(&[cmd("a", 0.3)]).is_ok());
        assert!(d.clear().is_ok());
    }
}
