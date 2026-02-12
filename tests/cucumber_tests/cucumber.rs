/// Usage: Set the environment variable `CUCUMBER_DEPLOYER_COMPOSE` to use the
/// Compose deployer. Otherwise, the Local deployer is used by default.
///
/// Example using docker compose deployer:
/// ```sh
/// CUCUMBER_DEPLOYER_COMPOSE=1 cargo run -p runner-examples --bin cucumber_auto -- --name "Run auto deployer smoke scenario"
/// ```
/// Example using local deployer:
/// ```sh
/// cargo run -p runner-examples --bin cucumber_auto --  --name "Run auto deployer smoke scenario"
/// ```
use std::io;
use std::{
    collections::HashMap,
    fs::OpenOptions,
    sync::{Arc, Mutex},
};

use cucumber::{World as _, WriterExt as _, writer, writer::Verbosity};
use logos_blockchain_tests::cucumber::{
    defaults::{
        ARTEFACTS, create_scenario_output_dir, get_feature_path, get_retries,
        init_logging_defaults, init_tracing,
    },
    world::{CucumberWorld, DeployerKind},
};

type ScenarioAttempts = Arc<Mutex<HashMap<String, u8>>>;

// Increment and return the attempt count for the given scenario. Counts
// are tracked per-scenario, and keyed by a combination of feature and
// scenario name.
#[expect(clippy::significant_drop_tightening, reason = "Compiler weirdness")]
fn increment_attempts(
    scenario_attempts: &ScenarioAttempts,
    feature: &str,
    scenario: &str,
) -> String {
    let mut guard = scenario_attempts
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = format!("{feature}::{scenario}");
    let entry = guard.entry(key).or_insert(0);
    *entry = entry.wrapping_add(1);
    format!("attempt_{}", *entry)
}

#[tokio::main]
async fn main() {
    println!("args: {:?}", std::env::args());

    let deployer = if std::env::var("CUCUMBER_DEPLOYER_COMPOSE").ok().is_some() {
        DeployerKind::Compose
    } else {
        DeployerKind::Local
    };
    println!("Running with '{deployer:?}'");

    init_logging_defaults();
    init_tracing();

    let scenario_attempts: ScenarioAttempts = Arc::new(Mutex::new(HashMap::new()));

    let output_dir = create_scenario_output_dir();
    let junit_xml_file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(output_dir.join("cucumber-output-junit.xml"))
        .inspect_err(|err| println!("Failed to open output file: {err}"))
        .expect("should create or open output file");
    let mut world = CucumberWorld::cucumber()
        // Re-outputs Failed steps for easier navigation.
        .repeat_failed()
        // .fail_fast() // Remove comment to enable fail-fast behavior for development
        .max_concurrent_scenarios(1)
        // Ensure that all the steps were covered.
        .fail_on_skipped()
        // Replaces Writer.
        .with_writer(
            writer::Summarize::new(writer::Basic::new(
                io::stdout(),
                // With `writer::Coloring::Auto`, cucumber treats the output as a TTY and using the
                // underlying termcolor/console behaviour that can rewrite/clear lines when
                // printing step statuses (✔ ...). That can visually clobber the
                // immediately adjacent tracing line, especially the one emitted
                // right as the step transitions from “running” to “passed”.
                writer::Coloring::Never,
                Verbosity::ShowWorldAndDocString,
            ))
            .tee::<CucumberWorld, _>(writer::JUnit::for_tee(junit_xml_file, 0))
            .normalized(),
        )
        // Sets a hook, executed on each Scenario before running all its Steps, including Background
        // ones.
        .before(move |feature, _rule, scenario, world| {
            Box::pin({
                let output_dir_clone = output_dir.clone();
                let scenario_attempts_clone = ScenarioAttempts::clone(&scenario_attempts);
                async move {
                    println!(
                        "\nStarting - {}: {} ({}: {})\n",
                        scenario.keyword, scenario.name, feature.keyword, feature.name,
                    );
                    world.set_deployer(deployer);
                    if let Err(err) = world.preflight(deployer) {
                        println!("Preflight failed for scenario '{}': {err}", scenario.name);
                    }

                    let run_attempt =
                        increment_attempts(&scenario_attempts_clone, &feature.name, &scenario.name);
                    let scenario_dir = output_dir_clone
                        .join(ARTEFACTS)
                        .join(&feature.name)
                        .join(scenario.name.trim().replace(' ', "_"))
                        .join(run_attempt);
                    world.set_scenario_base_dir(&scenario_dir, &deployer);
                }
            })
        });
    if let Some(retries) = get_retries()
        .inspect_err(|e| println!("{e}"))
        .expect("should parse retries")
    {
        // Makes failed Scenarios being retried the specified number of times.
        world = world.retries(retries);
    }

    // Runs Cucumber. Features sourced from a Parser are fed to a Runner, which
    // produces events handled by a Writer.
    world.run_and_exit(get_feature_path()).await;
}
