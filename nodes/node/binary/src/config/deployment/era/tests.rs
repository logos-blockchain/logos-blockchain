//! The era parameters of a deployment file are tagged with their version.

use std::collections::BTreeMap;

use lb_cryptarchia_engine::{
    Epoch,
    era::{EraVersion, MAX_ERAS_AFTER_GENESIS},
};
use lb_era_parameters::EraParameters;
use lb_utils::yaml::{OnUnknownKeys, deserialize_value_from_reader};

use super::{EraSchedule, EraScheduleError};
use crate::config::DeploymentSettings;

fn parameters() -> EraParameters {
    DeploymentSettings::default().eras.into_genesis()
}

fn yaml(parameters: &EraParameters) -> String {
    serde_yaml::to_string(parameters).unwrap()
}

#[test]
fn parameters_are_tagged_with_their_version() {
    let yaml = yaml(&parameters());
    assert!(yaml.starts_with("!V1\nblend:\n"), "{yaml}");
    let json = serde_json::to_string(&parameters()).unwrap();
    assert!(json.starts_with(r#"{"V1":{"blend":"#), "{json}");
}

#[test]
fn parameters_round_trip() {
    let parameters = parameters();
    let decoded: EraParameters = serde_yaml::from_str(&yaml(&parameters)).unwrap();
    assert_eq!(decoded.version(), EraVersion::V1);
    assert_eq!(yaml(&decoded), yaml(&parameters));
}

#[test]
fn parameters_without_a_known_version_tag_are_rejected() {
    let tagged = yaml(&parameters());
    let untagged = tagged.replacen("!V1\n", "", 1);
    assert!(serde_yaml::from_str::<EraParameters>(&untagged).is_err());
    let unknown = tagged.replacen("!V1\n", "!V0\n", 1);
    assert!(serde_yaml::from_str::<EraParameters>(&unknown).is_err());
}

#[test]
fn unknown_keys_are_reported_through_the_tag() {
    let tagged = yaml(&parameters());
    let with_unknown_key = tagged.replacen("!V1\n", "!V1\nsurprise: 1\n", 1);
    assert!(
        deserialize_value_from_reader::<EraParameters, _>(
            with_unknown_key.as_bytes(),
            OnUnknownKeys::Fail
        )
        .is_err()
    );
    assert!(
        deserialize_value_from_reader::<EraParameters, _>(tagged.as_bytes(), OnUnknownKeys::Fail)
            .is_ok()
    );
}

#[test]
fn a_schedule_of_more_eras_than_a_chain_can_number_is_rejected() {
    let parameters = parameters();
    let too_many = MAX_ERAS_AFTER_GENESIS + 1;
    let eras: BTreeMap<_, _> = (0..=u32::try_from(too_many).unwrap())
        .map(|first_epoch| (Epoch::new(first_epoch), parameters.clone()))
        .collect();

    assert_eq!(
        EraSchedule::try_from(eras).unwrap_err(),
        EraScheduleError::TooManyEras(too_many)
    );
}
