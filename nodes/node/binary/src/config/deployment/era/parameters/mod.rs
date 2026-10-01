use lb_cryptarchia_engine::era::EraVersion;
use serde::{Deserialize, Serialize};

pub mod v1;

mod codec;

/// The parameters an era defines, in the layout of its version: everything
/// every node on the chain must agree on while the era is in force.
///
/// Serialized as serde's externally tagged enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EraParameters {
    V1(v1::Parameters),
}

impl EraParameters {
    /// The version of the parameter set.
    #[must_use]
    pub const fn version(&self) -> EraVersion {
        match self {
            Self::V1(_) => EraVersion::V1,
        }
    }

    const fn tag(&self) -> u16 {
        self.version().tag()
    }
}

#[cfg(test)]
mod tests {
    use lb_cryptarchia_engine::era::EraVersion;
    use lb_utils::yaml::{OnUnknownKeys, deserialize_value_from_reader};

    use super::EraParameters;
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
            deserialize_value_from_reader::<EraParameters, _>(
                tagged.as_bytes(),
                OnUnknownKeys::Fail
            )
            .is_ok()
        );
    }
}
