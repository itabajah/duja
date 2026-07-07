//! The cross-backend [`crate::controller::BrightnessController`] contract.
//!
//! Every backend (fake now; real DDC/panel backends in later phases) must
//! satisfy [`run_controller_contract`]. The factory produces a fresh
//! controller for each scenario so cases never interfere.

// ---- specs first (TDD); implementation follows in the next commit ----

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::ControlError;
    use crate::testing::controller::FakeController;

    fn factory(scenario: Scenario) -> FakeController {
        match scenario {
            Scenario::Nominal => FakeController::new(),
            Scenario::Disconnected => FakeController::disconnected(),
            Scenario::ErrorThenOk => {
                let mut c = FakeController::new();
                c.push_error(ControlError::Timeout);
                c
            }
        }
    }

    #[test]
    fn fake_controller_satisfies_contract() {
        run_controller_contract(factory, 0);
    }
}
