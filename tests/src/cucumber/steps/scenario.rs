use cucumber::given;

use crate::cucumber::world::{CucumberWorld, NetworkKind, StepError, StepResult, parse_deployer};

#[given(expr = "deployer is {string}")]
#[expect(clippy::needless_pass_by_value, reason = "Required by Cucumber")]
fn deployer_is(world: &mut CucumberWorld, deployer: String) -> StepResult {
    world.set_deployer(parse_deployer(&deployer)?)
}

#[given(expr = "we have a CLI deployer specified")]
#[expect(clippy::needless_pass_by_ref_mut, reason = "Required by Cucumber")]
fn auto_deployer(world: &mut CucumberWorld) -> StepResult {
    let _unused = world
        .deployer
        .ok_or(StepError::MissingDeployer)
        .inspect_err(|e| {
            println!(
                "CLI deployer mode not specified, use '--deployer=compose' or '--deployer=local': {e}",
            );
        })?;
    Ok(())
}

#[given(expr = "topology has {int} validators")]
fn topology_has(world: &mut CucumberWorld, validators: usize) -> StepResult {
    world.set_topology(validators, NetworkKind::Star)
}
