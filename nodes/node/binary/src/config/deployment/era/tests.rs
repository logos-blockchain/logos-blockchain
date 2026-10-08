//! Each section of an era's parameters is tagged with its version, and an era
//! after genesis declares only the sections it changes.

use core::{num::NonZero, time::Duration};
use std::collections::BTreeMap;

use lb_cryptarchia_engine::era::MAX_ERAS_AFTER_GENESIS;
use lb_era_parameters::{ChangeError, EraChanges, EraParameters, Section};
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
fn sections_are_tagged_with_their_version() {
    let yaml = yaml(&parameters());
    for section in [
        "blend: !V1\n",
        "blocks: V1\n",
        "cryptarchia: !V1\n",
        "time: !V1\n",
    ] {
        assert!(yaml.contains(section), "{yaml}");
    }
}

#[test]
fn parameters_round_trip() {
    let parameters = parameters();
    let decoded: EraParameters = serde_yaml::from_str(&yaml(&parameters)).unwrap();
    assert_eq!(yaml(&decoded), yaml(&parameters));
}

#[test]
fn a_section_without_a_known_version_tag_is_rejected() {
    let tagged = yaml(&parameters());
    let untagged = tagged.replacen("time: !V1\n", "time:\n", 1);
    assert!(serde_yaml::from_str::<EraParameters>(&untagged).is_err());
    let unknown = tagged.replacen("time: !V1\n", "time: !V0\n", 1);
    assert!(serde_yaml::from_str::<EraParameters>(&unknown).is_err());
}

#[test]
fn unknown_keys_are_reported_through_the_tag() {
    let tagged = yaml(&parameters());
    let with_unknown_key = tagged.replacen("time: !V1\n", "time: !V1\n  surprise: 1\n", 1);
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

/// A schedule whose second era, from epoch 10, declares `second`.
fn schedule_yaml(second: &str) -> String {
    let genesis = yaml(&parameters()).replace('\n', "\n    ");
    format!("0:\n    {genesis}\n10:\n{second}")
}

#[test]
fn an_era_after_genesis_declares_only_the_sections_it_changes() {
    let schedule: EraSchedule =
        serde_yaml::from_str(&schedule_yaml("  time: !V1\n    slot_duration: '7.0'\n")).unwrap();
    let eras = schedule.resolve().unwrap();
    let [(_, _, genesis), (_, declared, second)] = eras.as_slice() else {
        panic!("the schedule has two eras");
    };

    assert!(declared.time.is_some() && declared.cryptarchia.is_none());
    assert_eq!(second.slot_duration(), Duration::from_secs(7));
    assert_eq!(second.epoch_length(), genesis.epoch_length());
}

#[test]
fn an_era_after_genesis_that_restates_a_section_is_rejected() {
    let slot_duration = parameters().slot_duration().as_secs();
    let restated = format!("  time: !V1\n    slot_duration: '{slot_duration}.0'\n");

    let error = serde_yaml::from_str::<EraSchedule>(&schedule_yaml(&restated)).unwrap_err();

    assert!(
        error.to_string().contains(
            &EraScheduleError::Changes {
                epoch: lb_cryptarchia_engine::Epoch::new(10),
                source: ChangeError::Unchanged(Section::Time),
            }
            .to_string()
        ),
        "{error}"
    );
}

#[test]
fn a_schedule_of_more_eras_than_a_chain_can_number_is_rejected() {
    let too_many = MAX_ERAS_AFTER_GENESIS + 1;
    let no_change = || EraChanges {
        blend: None,
        blocks: None,
        cryptarchia: None,
        time: None,
    };
    let after_genesis: BTreeMap<_, _> = (1..=u32::try_from(too_many).unwrap())
        .map(|first_epoch| (NonZero::new(first_epoch).unwrap(), no_change()))
        .collect();

    assert_eq!(
        EraSchedule::new(parameters(), after_genesis).unwrap_err(),
        EraScheduleError::TooManyEras(too_many)
    );
}
