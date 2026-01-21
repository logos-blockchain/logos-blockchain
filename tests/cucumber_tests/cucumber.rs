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
use std::{fs, io};

use cucumber::{World as _, WriterExt as _, writer, writer::Verbosity};
use logos_blockchain_tests::cucumber::{
    defaults::{
        create_scenario_output_dir, get_feature_path, init_logging_defaults,
        init_node_log_dir_defaults, init_tracing,
    },
    world::{CucumberWorld, DeployerKind},
};

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
    init_node_log_dir_defaults(deployer);
    init_tracing();

    let output_dir = create_scenario_output_dir();
    let junit_xml_file = fs::File::create(output_dir.join("cucumber-output-junit.xml")).unwrap();
    let world = CucumberWorld::cucumber()
        .repeat_failed()
        .max_concurrent_scenarios(1)
        .fail_on_skipped()
        // .fail_fast() // Remove comment to enable fail-fast behavior for development
        .with_writer(
            writer::Summarize::new(writer::Basic::new(
                io::stdout(),
                writer::Coloring::Auto,
                Verbosity::ShowWorldAndDocString,
            ))
            .tee::<CucumberWorld, _>(writer::JUnit::for_tee(junit_xml_file, 0))
            .normalized(),
        )
        .before(move |feature, _rule, scenario, world| {
            Box::pin(async move {
                println!(
                    "\nStarting - {}: {} ({}: {})\n",
                    scenario.keyword, scenario.name, feature.keyword, feature.name,
                ); // This will be printed into the stdout_buffer
                if let Err(e) = world.set_deployer(deployer) {
                    panic!("Failed to set deployer: {e}");
                }
            })
        });

    world.run_and_exit(get_feature_path()).await;
}
