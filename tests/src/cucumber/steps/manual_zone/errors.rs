use cucumber::gherkin::Step;
use tracing::warn;

use super::support::ZoneTestError;
use crate::cucumber::{error::StepError, steps::TARGET};

pub(super) fn zone_step_error(step: &Step, error: &ZoneTestError) -> StepError {
    log_zone_error(step, error);

    StepError::LogicalError {
        message: error.to_string(),
    }
}

pub(super) fn log_step_error<T>(step: &Step, result: Result<T, StepError>) -> Result<T, StepError> {
    result.inspect_err(|error| {
        warn!(target: TARGET, "Step `{}` error: {error}", step.value);
    })
}

pub(super) fn log_zone_error(step: &Step, error: &ZoneTestError) {
    warn!(target: TARGET, "Step `{}` error: {error}", step.value);
}
