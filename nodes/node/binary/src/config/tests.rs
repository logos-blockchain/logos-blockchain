use clap::Parser as _;

use crate::config::CliArgs;

#[test]
fn parse_config_path() {
    let parsed_args = CliArgs::parse_from(["", "test_cfg.yaml"]);
    assert_eq!(parsed_args.config_path().to_str().unwrap(), "test_cfg.yaml");
}
