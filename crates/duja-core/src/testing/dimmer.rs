//! A scriptable [`Dimmer`] fake: records every applied batch and each `clear`,
//! with an injectable error queue.

use std::collections::VecDeque;

use crate::dimmer::{DimCommand, Dimmer, DimmerError};

/// A deterministic, scriptable [`Dimmer`] for tests.
///
/// Every [`apply`](Dimmer::apply) records the *sanitized* command batch (so
/// assertions see the same clamped values a real backend would act on) and
/// updates [`current`](Self::current) to the visible-overlay subset; every
/// [`clear`](Dimmer::clear) is counted and empties the current state. Errors
/// queued with [`push_error`](Self::push_error) are returned by the next
/// operations, one per op, then normal behaviour resumes — a failure never
/// poisons later calls, and the batch that failed is **not** recorded.
#[derive(Debug, Default)]
pub struct FakeDimmer {
    batches: Vec<Vec<DimCommand>>,
    current: Vec<DimCommand>,
    clears: usize,
    errors: VecDeque<DimmerError>,
}

impl FakeDimmer {
    /// A fresh fake with no history and no scripted failures.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue an error to be returned by the next [`apply`](Dimmer::apply) or
    /// [`clear`](Dimmer::clear) call.
    pub fn push_error(&mut self, err: DimmerError) {
        self.errors.push_back(err);
    }

    /// Every recorded [`apply`](Dimmer::apply) batch, in order (sanitized).
    #[must_use]
    pub fn batches(&self) -> &[Vec<DimCommand>] {
        &self.batches
    }

    /// The most recent applied batch, or `None` if nothing has been applied.
    #[must_use]
    pub fn last_batch(&self) -> Option<&[DimCommand]> {
        self.batches.last().map(Vec::as_slice)
    }

    /// The current visible-overlay state: the sanitized commands from the last
    /// successful [`apply`](Dimmer::apply) whose alpha is above zero. Empty
    /// after a [`clear`](Dimmer::clear).
    #[must_use]
    pub fn current(&self) -> &[DimCommand] {
        &self.current
    }

    /// How many times [`clear`](Dimmer::clear) has succeeded.
    #[must_use]
    pub fn clear_count(&self) -> usize {
        self.clears
    }

    /// Pop a scripted error, if any (one consumed per operation).
    fn take_error(&mut self) -> Option<DimmerError> {
        self.errors.pop_front()
    }
}

impl Dimmer for FakeDimmer {
    fn apply(&mut self, commands: &[DimCommand]) -> Result<(), DimmerError> {
        if let Some(err) = self.take_error() {
            return Err(err);
        }
        let sanitized: Vec<DimCommand> = commands.iter().map(DimCommand::sanitized).collect();
        self.current = sanitized
            .iter()
            .filter(|c| c.has_overlay())
            .cloned()
            .collect();
        self.batches.push(sanitized);
        Ok(())
    }

    fn clear(&mut self) -> Result<(), DimmerError> {
        if let Some(err) = self.take_error() {
            return Err(err);
        }
        self.current.clear();
        self.clears = self.clears.saturating_add(1);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::continuum::MAX_ALPHA;
    use crate::dimmer::DisplayBounds;
    use crate::id::StableDisplayId;

    fn id(serial: &str) -> StableDisplayId {
        StableDisplayId::from_parts("AAA", 0x0001, Some(serial)).unwrap()
    }

    fn cmd(serial: &str, alpha: f32) -> DimCommand {
        DimCommand::new(id(serial), DisplayBounds::new(0, 0, 100, 100), alpha, None)
    }

    #[test]
    fn records_batches_in_order() {
        let mut d = FakeDimmer::new();
        d.apply(&[cmd("a", 0.5)]).unwrap();
        d.apply(&[cmd("a", 0.5), cmd("b", 0.2)]).unwrap();
        assert_eq!(d.batches().len(), 2);
        assert_eq!(d.batches().first().map(Vec::len), Some(1));
        assert_eq!(d.last_batch().unwrap().len(), 2);
    }

    #[test]
    fn records_sanitized_values() {
        let mut d = FakeDimmer::new();
        d.apply(&[DimCommand {
            id: id("a"),
            bounds: DisplayBounds::new(0, 0, 1, 1),
            overlay_alpha: 5.0,
            gamma: None,
        }])
        .unwrap();
        let recorded = d.last_batch().unwrap().first().map(|c| c.overlay_alpha);
        assert!((recorded.unwrap() - MAX_ALPHA).abs() < f32::EPSILON);
    }

    #[test]
    fn current_holds_only_visible_overlays() {
        let mut d = FakeDimmer::new();
        d.apply(&[cmd("a", 0.5), cmd("b", 0.0)]).unwrap();
        assert_eq!(d.current().len(), 1);
        assert_eq!(d.current().first().map(|c| c.id.clone()), Some(id("a")));
    }

    #[test]
    fn clear_empties_current_and_counts() {
        let mut d = FakeDimmer::new();
        d.apply(&[cmd("a", 0.5)]).unwrap();
        d.clear().unwrap();
        assert!(d.current().is_empty());
        assert_eq!(d.clear_count(), 1);
    }

    #[test]
    fn injected_error_is_consumed_once_and_batch_not_recorded() {
        let mut d = FakeDimmer::new();
        d.push_error(DimmerError::Backend);
        assert!(matches!(
            d.apply(&[cmd("a", 0.5)]),
            Err(DimmerError::Backend)
        ));
        assert_eq!(d.batches().len(), 0);
        assert!(d.apply(&[cmd("a", 0.5)]).is_ok());
        assert_eq!(d.batches().len(), 1);
    }

    #[test]
    fn injected_error_applies_to_clear_too() {
        let mut d = FakeDimmer::new();
        d.push_error(DimmerError::Os("boom".to_owned()));
        assert!(matches!(d.clear(), Err(DimmerError::Os(_))));
        assert_eq!(d.clear_count(), 0);
        assert!(d.clear().is_ok());
        assert_eq!(d.clear_count(), 1);
    }

    #[test]
    fn is_usable_as_a_trait_object() {
        let mut d: Box<dyn Dimmer> = Box::new(FakeDimmer::new());
        assert!(d.apply(&[cmd("a", 0.3)]).is_ok());
        assert!(d.clear().is_ok());
    }
}
