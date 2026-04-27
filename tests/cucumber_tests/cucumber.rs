/// Usage: Set `CUCUMBER_DEPLOYER_K8S` or `CUCUMBER_DEPLOYER_COMPOSE` to choose
/// the deployer. Otherwise, the Local deployer is used by default.
///
/// Example using docker compose deployer:
/// ```sh
/// CUCUMBER_DEPLOYER_COMPOSE=1 cargo run -p runner-examples --bin cucumber_auto -- --name "Run auto deployer smoke scenario"
/// ```
/// Example using k8s deployer:
/// ```sh
/// CUCUMBER_DEPLOYER_K8S=1 cargo run -p runner-examples --bin cucumber_auto -- --name "Run auto deployer smoke scenario"
/// ```
/// Example using local deployer:
/// ```sh
/// cargo run -p runner-examples --bin cucumber_auto --  --name "Run auto deployer smoke scenario"
/// ```
use std::io;
use std::{
    collections::HashMap,
    fs::OpenOptions,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use cucumber::{
    StatsWriter as _, World as _, WriterExt as _, event::ScenarioFinished, writer,
    writer::Verbosity,
};
use lb_testing_framework::{
    hash_str, is_truthy_env, reap_all_stale_port_blocks, record_system_monitor_event,
    register_system_monitor_output_file, release_reserved_port_block,
    unregister_system_monitor_output_file,
};
use logos_blockchain_tests::cucumber::{
    defaults::{
        ARTEFACTS, CUCUMBER_DEPLOYER_COMPOSE, CUCUMBER_DEPLOYER_K8S,
        CUCUMBER_REMOVE_ARTEFACTS_IF_SUCCESSFUL, create_scenario_output_dir, get_feature_path,
        get_retries, init_logging_defaults, init_tracing,
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
    reap_all_stale_port_blocks();
    println!("args: {:?}", std::env::args());

    let deployer = selected_deployer();
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
                    prepare_world_for_scenario(
                        world,
                        deployer,
                        &output_dir_clone,
                        &scenario_attempts_clone,
                        &feature.name,
                        &scenario.name,
                    );
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

    let failed = world
        .after(|feature, _rule, scenario, scenario_finished, world| {
            Box::pin(async move {
                // Runs after the scenario has completed; useful for capturing final state/logs.
                println!(
                    "\nFinished - {}: {} ({}: {})\n",
                    scenario.keyword, scenario.name, feature.keyword, feature.name,
                );

                if let Some(world) = world {
                    let path = world.scenario_base_dir.join("debug_dump_file.log");
                    if let Some(parent) = path.parent() {
                        let _unused = std::fs::create_dir_all(parent);
                    }
                    let _unused = std::fs::write(&path, world.full_debug_info_string());

                    if matches!(scenario_finished, ScenarioFinished::StepPassed)
                        && is_truthy_env(CUCUMBER_REMOVE_ARTEFACTS_IF_SUCCESSFUL)
                    {
                        println!(
                            "Env var '{CUCUMBER_REMOVE_ARTEFACTS_IF_SUCCESSFUL}' set, removing all \
                            artefacts\n"
                        );
                        if let Err(e) = world.clear_scenario_artifacts() {
                            println!("{e}");
                        }
                    }

                    unregister_system_monitor_output_file(
                        &world.scenario_base_dir.join("system_stats.ndjson"),
                    );
                }
            })
        })
        // Runs Cucumber. Features sourced from a Parser are fed to a Runner, which
        // produces events handled by a Writer.
        .run(get_feature_path_for_deployer(deployer))
        .await;

    // Clean up manually reserved handshake port block files for this process
    release_reserved_port_block();

    if failed.execution_has_failed() {
        std::process::exit(1);
    }
}

fn selected_deployer() -> DeployerKind {
    // The k8s deployer uses the active Kubernetes client configuration on the
    // machine running the test, so it targets the currently configured cluster
    // and context rather than provisioning one itself. To use custom images,
    // set `LOGOS_BLOCKCHAIN_K8S_NODE_IMAGE` for node pods and optionally
    // `LOGOS_BLOCKCHAIN_K8S_BOOTSTRAP_IMAGE` for the cfgsync/bootstrap pod.
    // If those are unset, the runner falls back to `LOGOS_BLOCKCHAIN_TESTNET_IMAGE`
    // or the default local node and cfgsync images built by the runtime
    // docker scripts under `tests/testing_framework/assets/runtime/scripts/docker`.
    if is_truthy_env(CUCUMBER_DEPLOYER_K8S) {
        return DeployerKind::K8s;
    }

    if is_truthy_env(CUCUMBER_DEPLOYER_COMPOSE) {
        return DeployerKind::Compose;
    }

    DeployerKind::Local
}

fn get_feature_path_for_deployer(deployer: DeployerKind) -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let feature_path = match deployer {
        DeployerKind::K8s => manifest_dir.join("cucumber_tests/features_k8s"),
        DeployerKind::Local | DeployerKind::Compose => get_feature_path(),
    };
    println!("Feature path:      {}", feature_path.display());
    feature_path
}

fn prepare_world_for_scenario(
    world: &mut CucumberWorld,
    deployer: DeployerKind,
    output_dir: &Path,
    scenario_attempts: &ScenarioAttempts,
    feature_name: &str,
    scenario_name: &str,
) {
    world.set_deployer(deployer);

    if let Err(err) = world.preflight(deployer) {
        println!("Preflight failed for scenario '{scenario_name}': {err}");
    }

    let scenario_dir =
        scenario_output_dir(output_dir, scenario_attempts, feature_name, scenario_name);

    if let Err(err) = std::fs::create_dir_all(&scenario_dir) {
        println!(
            "Failed to create scenario artifact directory '{}': {err}",
            scenario_dir.display()
        );
    }

    register_system_monitor_output_file(&scenario_dir.join("system_stats.ndjson"));
    record_system_monitor_event(
        "cucumber_scenario_prepared",
        scenario_dir.display().to_string(),
    );

    world.set_scenario_base_dir(&scenario_dir, &deployer);
    world.apply_deployment_config_override_path();

    let started_at_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    let raw_context = format!("{}::{started_at_ns}", scenario_dir.display());
    world.set_test_context(hash_str(&raw_context));
}

fn scenario_output_dir(
    output_dir: &Path,
    scenario_attempts: &ScenarioAttempts,
    feature_name: &str,
    scenario_name: &str,
) -> PathBuf {
    let run_attempt = increment_attempts(scenario_attempts, feature_name, scenario_name);

    output_dir
        .join(ARTEFACTS)
        .join(feature_name)
        .join(scenario_name.trim().replace(' ', "_"))
        .join(run_attempt)
}
