use std::{num::NonZero, time::Duration};

use cucumber::{given, when};

use crate::cucumber::{
    error::{StepError, StepResult},
    world::CucumberWorld,
};

#[given(expr = "the cluster uses cryptarchia security parameter {int}")]
#[when(expr = "the cluster uses cryptarchia security parameter {int}")]
fn step_set_cryptarchia_security_param(
    world: &mut CucumberWorld,
    security_param: u32,
) -> StepResult {
    let security_param = NonZero::new(security_param).ok_or(StepError::InvalidArgument {
        message: "cryptarchia security parameter must be greater than 0".to_owned(),
    })?;

    world.set_cryptarchia_security_param(security_param);

    Ok(())
}

#[given(expr = "the cluster uses prolonged bootstrap period of {int} seconds")]
#[when(expr = "the cluster uses prolonged bootstrap period of {int} seconds")]
const fn step_set_prolonged_bootstrap_period(
    world: &mut CucumberWorld,
    bootstrap_period_secs: u64,
) {
    world.set_prolonged_bootstrap_period(Duration::from_secs(bootstrap_period_secs));
}
