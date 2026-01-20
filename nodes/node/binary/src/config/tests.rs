use clap::Parser as _;

use crate::config::CliArgs;

#[test]
fn start_all_service_groups() {
    let parsed_args = CliArgs::parse_from(["", "test_cfg.yaml"]);
    assert!(parsed_args.must_blend_service_group_start());
    assert!(parsed_args.must_da_service_group_start());
}

#[test]
fn start_blend_service_group_only() {
    let parsed_args = CliArgs::parse_from(["", "test_cfg.yaml", "--blend-service-group"]);
    assert!(parsed_args.must_blend_service_group_start());
    assert!(!parsed_args.must_da_service_group_start());
}

#[test]
fn start_da_service_group_only() {
    let parsed_args = CliArgs::parse_from(["", "test_cfg.yaml", "--da-service-group"]);
    assert!(!parsed_args.must_blend_service_group_start());
    assert!(parsed_args.must_da_service_group_start());
}
