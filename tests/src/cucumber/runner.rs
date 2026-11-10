use std::{
    collections::HashMap,
    fmt::Debug,
    fs::OpenOptions,
    io,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use cucumber::{
    StatsWriter as _, World, WriterExt as _,
    codegen::WorldInventory,
    event::ScenarioFinished,
    gherkin::{Feature, Rule, Scenario},
    step::Collection,
    writer,
    writer::Verbosity,
};
use lb_testing_framework::{
    configs::deployment::NodeBinaryProfile, ensure_node_binary_built, hash_str, is_truthy_env,
    record_system_monitor_event, register_system_monitor_output_file, release_reserved_port_block,
    resolve_automatic_genesis_time, unregister_system_monitor_output_file,
};

use crate::cucumber::{
    defaults::{
        ARTEFACTS, CUCUMBER_DEPLOYER_COMPOSE, CUCUMBER_DEPLOYER_K8S,
        CUCUMBER_REMOVE_ARTEFACTS_IF_SUCCESSFUL, MAX_CUCUMBER_CONCURRENT_SCENARIOS,
        create_scenario_output_dir, get_feature_path, get_retries, init_logging_defaults,
        init_tracing,
    },
    deployment::LocalImplementation,
    error::StepResult,
    world::{CucumberWorld, DeployerKind},
};

type ScenarioAttempts = Arc<Mutex<HashMap<String, u8>>>;

/// Releases a suite's resources after every scenario, including failed ones.
/// Client suites stop their clients before stopping the nodes they use.
pub trait ScenarioCleanup {
    fn cleanup(&mut self) -> impl Future<Output = StepResult>;
}

impl ScenarioCleanup for CucumberWorld {
    async fn cleanup(&mut self) -> StepResult {
        self.stop_background_activity().await
    }
}

// Get the maximum number of concurrent scenarios from env var, defaults to 1
fn get_max_concurrent_scenarios() -> usize {
    std::env::var(MAX_CUCUMBER_CONCURRENT_SCENARIOS)
        .ok()
        .and_then(|val| val.parse().ok())
        .unwrap_or(1)
}

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

/// Run the existing feature suite with the supplied local implementation.
#[expect(
    clippy::future_not_send,
    reason = "Cucumber uses non-Send futures; entry points await the runner directly"
)]
pub async fn run(implementation: LocalImplementation) -> ExitCode {
    run_suite::<CucumberWorld>(
        implementation,
        get_feature_path_for_deployer(selected_deployer()),
        CucumberWorld::collection(),
        CucumberWorld::full_debug_info_string,
    )
    .await
}

/// Runs a suite using the shared node setup, diagnostics and reporting.
///
/// Hooks access the embedded node world through `AsMut`. The suite supplies
/// its step collection and debug formatter so client state stays in its own
/// crate while still appearing in scenario diagnostics. `ScenarioCleanup` runs
/// after those diagnostics are captured and before artifacts can be removed.
#[expect(
    clippy::future_not_send,
    reason = "Cucumber uses non-Send futures; entry points await the runner directly"
)]
pub async fn run_suite<W>(
    implementation: LocalImplementation,
    feature_path: PathBuf,
    steps: Collection<W>,
    debug_info: fn(&W) -> String,
) -> ExitCode
where
    W: World + WorldInventory + AsMut<CucumberWorld> + ScenarioCleanup + Debug,
{
    println!("args: {:?}", std::env::args());

    let deployer = selected_deployer();
    if deployer != DeployerKind::Local {
        implementation
            .require_logos("non-local deployment")
            .expect("supported implementation");
    }
    println!(
        "Running with '{deployer:?}' and '{}'",
        implementation.name()
    );

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
    let mut world = W::cucumber()
        .steps(steps)
        // Re-outputs Failed steps for easier navigation.
        .repeat_failed()
        // .fail_fast() // Remove comment to enable fail-fast behavior for development
        .max_concurrent_scenarios(get_max_concurrent_scenarios())
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
            .tee::<W, _>(writer::JUnit::for_tee(junit_xml_file, 0))
            .normalized(),
        )
        // Ensure that all the steps were covered. Keep this after `with_writer`, which replaces
        // the runner's writer and would otherwise discard the skipped-step failure wrapper.
        .fail_on_skipped()
        // Sets a hook, executed on each Scenario before running all its Steps, including Background
        // ones.
        .before(move |feature, rule, scenario, world| {
            Box::pin({
                let output_dir_clone = output_dir.clone();
                let scenario_attempts_clone = ScenarioAttempts::clone(&scenario_attempts);
                let profile = NodeBinaryProfile::from_env_or(scenario_binary_profile(
                    feature, rule, scenario,
                ));
                async move {
                    println!(
                        "\nStarting - {}: {} ({}: {})\n",
                        scenario.keyword, scenario.name, feature.keyword, feature.name,
                    );
                    let world = world.as_mut();
                    world.cluster.implementation = implementation;
                    prepare_world_for_scenario(
                        world,
                        deployer,
                        &output_dir_clone,
                        &scenario_attempts_clone,
                        profile,
                        &feature.name,
                        &scenario.name,
                    )
                    .await;
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
        .after(move |feature, _rule, scenario, scenario_finished, world| {
            Box::pin(async move {
                // Runs after the scenario has completed; useful for capturing final state/logs.
                println!(
                    "\nFinished - {}: {} ({}: {})\n",
                    scenario.keyword, scenario.name, feature.keyword, feature.name,
                );

                if let Some(world) = world {
                    finish_scenario(
                        world,
                        matches!(scenario_finished, ScenarioFinished::StepPassed),
                        debug_info,
                    )
                    .await;
                }
            })
        })
        // Runs Cucumber. Features sourced from a Parser are fed to a Runner, which
        // produces events handled by a Writer.
        .run(feature_path)
        .await;

    // Release this process's reserved port block before exiting.
    release_reserved_port_block();

    if failed.execution_has_failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
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
    // docker scripts under
    // `tests/tf_integration/logos/assets/runtime/scripts/docker`.
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

async fn prepare_world_for_scenario(
    world: &mut CucumberWorld,
    deployer: DeployerKind,
    output_dir: &Path,
    scenario_attempts: &ScenarioAttempts,
    profile: NodeBinaryProfile,
    feature_name: &str,
    scenario_name: &str,
) {
    world.set_deployer(deployer);
    world.lifecycle.node_binary_profile = Some(profile.clone());
    let requires_binary =
        deployer.requires_local_node_binary() && world.cluster.implementation.is_logos();
    assign_genesis_after_resolution(world, async {
        if requires_binary {
            let binary = ensure_node_binary_built(&profile).await.map_err(|error| format!("Failed to resolve {profile:?} node binary before assigning genesis time: {error}"))?;
            tracing::info!(?profile, path = %binary.display(), "Resolved node binary before scenario genesis");
        }
        Ok(())
    }).await.expect("scenario binary and genesis preparation failed");

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
    if feature_name == "Happy-path era boundaries" {
        crate::cucumber::steps::nodes::era_boundaries::initialize_evidence_file(&scenario_dir)
            .unwrap_or_else(|error| {
                panic!("Failed to initialize era-boundary evidence for scenario `{scenario_name}`: {error}")
            });
    }
    world.set_scenario_name(scenario_name);
    world.apply_deployment_config_override_path();

    let started_at_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    let raw_context = format!("{}::{started_at_ns}", scenario_dir.display());
    world.set_test_context(hash_str(&raw_context));
}

async fn assign_genesis_after_resolution(
    world: &mut CucumberWorld,
    resolution: impl Future<Output = Result<(), String>>,
) -> Result<(), String> {
    resolution.await?;
    if let Some(genesis) = world.lifecycle.genesis_time {
        if world.lifecycle.explicit_genesis_time
            && time::OffsetDateTime::from(genesis) < time::OffsetDateTime::now_utc()
        {
            return Err(format!(
                "explicit genesis time {genesis:?} has expired during binary resolution"
            ));
        }
    } else {
        world.set_genesis_time(resolve_automatic_genesis_time());
    }
    Ok(())
}

fn scenario_binary_profile(
    feature: &Feature,
    rule: Option<&Rule>,
    scenario: &Scenario,
) -> NodeBinaryProfile {
    let mut steps = feature
        .background
        .iter()
        .flat_map(|background| &background.steps)
        .chain(
            rule.into_iter()
                .flat_map(|rule| rule.background.iter())
                .flat_map(|background| &background.steps),
        )
        .chain(&scenario.steps);
    if steps.any(|step| {
        step.value
            .starts_with("I will have tokio console profile nodes")
    }) {
        NodeBinaryProfile::TokioConsole
    } else {
        NodeBinaryProfile::Normal
    }
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

async fn finish_scenario<W: AsMut<CucumberWorld> + ScenarioCleanup>(
    world: &mut W,
    successful: bool,
    debug_info: fn(&W) -> String,
) {
    let debug_info = debug_info(world);
    let cleanup = world.cleanup().await;
    let world = world.as_mut();

    let path = world
        .lifecycle
        .scenario_base_dir
        .join("debug_dump_file.log");
    if let Some(parent) = path.parent() {
        let _unused = std::fs::create_dir_all(parent);
    }
    let _unused = std::fs::write(&path, debug_info);

    if successful && cleanup.is_ok() && is_truthy_env(CUCUMBER_REMOVE_ARTEFACTS_IF_SUCCESSFUL) {
        println!(
            "Env var '{CUCUMBER_REMOVE_ARTEFACTS_IF_SUCCESSFUL}' set, removing all \
            artefacts\n"
        );
        if let Err(e) = world.clear_scenario_artifacts() {
            println!("{e}");
        }
    }

    unregister_system_monitor_output_file(
        &world
            .lifecycle
            .scenario_base_dir
            .join("system_stats.ndjson"),
    );

    // Cucumber reports an after-hook panic as a scenario failure. Keep the
    // diagnostics and artifacts above even when cleanup fails.
    cleanup.expect("scenario cleanup failed");
}

#[cfg(test)]
mod tests {
    use std::{fs::read_to_string, future::ready, panic::AssertUnwindSafe};

    use futures::FutureExt as _;
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn genesis_is_assigned_after_resolution_and_explicit_times_are_preserved() {
        let mut world = CucumberWorld::default();
        let resolution_started = time::OffsetDateTime::now_utc();
        assign_genesis_after_resolution(&mut world, async {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            Ok(())
        })
        .await
        .expect("binary resolution");
        assert!(
            time::OffsetDateTime::from(world.lifecycle.genesis_time.unwrap())
                >= resolution_started.replace_nanosecond(0).unwrap()
        );

        let future = time::OffsetDateTime::now_utc() + time::Duration::seconds(60);
        let genesis = lb_core::mantle::GenesisTime::try_from(future).unwrap();
        world.set_genesis_time(genesis);
        world.lifecycle.explicit_genesis_time = true;
        assign_genesis_after_resolution(&mut world, async { Ok(()) })
            .await
            .unwrap();
        assert_eq!(world.lifecycle.genesis_time, Some(genesis));

        world.set_genesis_time(lb_core::mantle::GenesisTime::new(1));
        assert!(
            assign_genesis_after_resolution(&mut world, async { Ok(()) })
                .await
                .unwrap_err()
                .contains("expired")
        );
    }
    use crate::cucumber::error::StepError;

    #[derive(Default)]
    struct CleanupWorld {
        node: CucumberWorld,
        cleaned: bool,
        fail_cleanup: bool,
    }

    impl AsMut<CucumberWorld> for CleanupWorld {
        fn as_mut(&mut self) -> &mut CucumberWorld {
            &mut self.node
        }
    }

    impl ScenarioCleanup for CleanupWorld {
        fn cleanup(&mut self) -> impl Future<Output = StepResult> {
            self.cleaned = true;

            if self.fail_cleanup {
                return ready(Err(StepError::StepFail {
                    message: "cleanup error".to_owned(),
                }));
            }

            ready(Ok(()))
        }
    }

    fn debug_info(world: &CleanupWorld) -> String {
        format!("cleaned: {}", world.cleaned)
    }

    #[tokio::test]
    async fn cleanup_runs_after_passed_and_failed_scenarios() {
        for successful in [true, false] {
            let directory = tempdir().expect("temporary directory");
            let mut world = CleanupWorld::default();
            world.node.lifecycle.scenario_base_dir = directory.path().to_owned();

            finish_scenario(&mut world, successful, debug_info).await;

            assert!(world.cleaned);
        }
    }

    #[tokio::test]
    async fn cleanup_failure_keeps_diagnostics_and_fails_the_hook() {
        let directory = tempdir().expect("temporary directory");
        let mut world = CleanupWorld {
            fail_cleanup: true,
            ..CleanupWorld::default()
        };
        world.node.lifecycle.scenario_base_dir = directory.path().to_owned();

        let result = AssertUnwindSafe(finish_scenario(&mut world, true, debug_info))
            .catch_unwind()
            .await;

        assert!(result.is_err());
        assert!(world.cleaned);
        assert_eq!(
            read_to_string(directory.path().join("debug_dump_file.log"))
                .expect("diagnostics are preserved"),
            "cleaned: false",
        );
    }
}
