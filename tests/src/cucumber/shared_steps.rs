use std::sync::LazyLock;

use cucumber::{
    World as _,
    codegen::{LocalBoxFuture, StepConstructor as _, WorldInventory, inventory},
    step::{Collection, Context, Step},
};

use super::world::CucumberWorld;

static NODE_STEPS: LazyLock<Collection<CucumberWorld>> = LazyLock::new(CucumberWorld::collection);

// The outer suite matched a node-step pattern, but its world has a different
// type. Find the original callback and its parsed arguments, then run it on
// the embedded CucumberWorld returned by AsMut.
fn forward_node<W: AsMut<CucumberWorld>>(
    world: &mut W,
    context: Context,
) -> LocalBoxFuture<'_, ()> {
    Box::pin(async move {
        let (step, _, _, context) = NODE_STEPS
            .find(&context.step)
            .expect("node step must be unambiguous")
            .expect("node step was registered");

        step(world.as_mut(), context).await;
    })
}

/// Adds node setup and assertion steps to a client suite's own steps.
pub fn with_node_steps<W: AsMut<CucumberWorld>>(steps: Collection<W>) -> Collection<W> {
    register_steps::<W, CucumberWorld>(steps, forward_node)
}

/// Registers the source world's step patterns for another world type.
///
/// The original callbacks accept `&mut S`, so they cannot run directly on `W`.
/// Each pattern instead calls `forward`, which finds the original callback
/// and runs it on the embedded `S`. Keeping the original source locations
/// lets Cucumber report where each step was defined.
pub fn register_steps<W, S: WorldInventory>(
    mut steps: Collection<W>,
    forward: Step<W>,
) -> Collection<W> {
    for entry in inventory::iter::<S::Given> {
        let (location, regex, _) = entry.inner();
        steps = steps.given(Some(location), regex(), forward);
    }
    for entry in inventory::iter::<S::When> {
        let (location, regex, _) = entry.inner();
        steps = steps.when(Some(location), regex(), forward);
    }
    for entry in inventory::iter::<S::Then> {
        let (location, regex, _) = entry.inner();
        steps = steps.then(Some(location), regex(), forward);
    }

    steps
}
