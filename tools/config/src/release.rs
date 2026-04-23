use lb_libp2p::{libp2p::StreamProtocol as Libp2pStreamProtocol, protocol_name::StreamProtocol};

const PROTOCOL_NAMESPACE_VAR: &str = "LOGOS_BLOCKCHAIN_PROTOCOL_NAMESPACE";
const PROTOCOL_TYPE_VAR: &str = "LOGOS_BLOCKCHAIN_PROTOCOL_TYPE";
const PROTOCOL_RELEASE_VAR: &str = "LOGOS_BLOCKCHAIN_PROTOCOL_RELEASE";

pub struct ProtocolIdentity {
    namespace: String,
}

impl ProtocolIdentity {
    #[must_use]
    pub fn from_env(default_namespace: &str) -> Self {
        if let Some(namespace) = env_protocol_value(PROTOCOL_NAMESPACE_VAR) {
            return Self { namespace };
        }

        let protocol_type = env_protocol_value(PROTOCOL_TYPE_VAR);
        let protocol_release = env_protocol_value(PROTOCOL_RELEASE_VAR);

        let namespace = match (protocol_type, protocol_release) {
            (None, None) => default_namespace.to_owned(),
            (protocol_type, protocol_release) => {
                let mut namespace = "logos-blockchain".to_owned();

                if let Some(protocol_type) = protocol_type {
                    namespace.push('-');
                    namespace.push_str(&protocol_type);
                }

                if let Some(protocol_release) = protocol_release {
                    namespace.push('-');
                    namespace.push_str(&protocol_release);
                }

                namespace
            }
        };

        Self { namespace }
    }

    #[must_use]
    pub fn stream_protocol(&self, suffix: &str) -> StreamProtocol {
        Libp2pStreamProtocol::try_from_owned(self.protocol_name(suffix))
            .expect("protocol name should be valid")
            .into()
    }

    #[must_use]
    pub fn protocol_name(&self, suffix: &str) -> String {
        format!("/{}/{suffix}", self.namespace)
    }
}

fn env_protocol_value(name: &str) -> Option<String> {
    let value = std::env::var(name).ok()?;
    let trimmed = value.trim().trim_matches('/').to_owned();

    (!trimmed.is_empty()).then_some(trimmed)
}

#[cfg(test)]
mod tests {
    use std::sync::{LazyLock, Mutex};

    use super::*;

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    #[test]
    fn uses_default_namespace_when_env_is_unset() {
        with_protocol_env(None, None, None, || {
            let identity = ProtocolIdentity::from_env("integration/logos-blockchain");

            assert_eq!(
                identity.protocol_name("identify/1.0.0"),
                "/integration/logos-blockchain/identify/1.0.0"
            );
            assert_eq!(
                identity.stream_protocol("kad/1.0.0").as_ref(),
                "/integration/logos-blockchain/kad/1.0.0"
            );
        });
    }

    #[test]
    fn namespace_override_takes_precedence_over_type_and_release() {
        with_protocol_env(
            Some(" /logos-blockchain-testnet-v0.1.2/ "),
            Some("ignored"),
            Some("ignored"),
            || {
                let identity = ProtocolIdentity::from_env("integration/logos-blockchain");

                assert_eq!(
                    identity.protocol_name("chainsync/1.0.0"),
                    "/logos-blockchain-testnet-v0.1.2/chainsync/1.0.0"
                );
            },
        );
    }

    #[test]
    fn derives_namespace_from_type_and_release() {
        with_protocol_env(None, Some("testnet"), Some("v0.1.2"), || {
            let identity = ProtocolIdentity::from_env("integration/logos-blockchain");

            assert_eq!(
                identity.protocol_name("cryptarchia/proto/1.0.0"),
                "/logos-blockchain-testnet-v0.1.2/cryptarchia/proto/1.0.0"
            );
        });
    }

    #[test]
    fn skips_empty_type_or_release_values() {
        with_protocol_env(None, Some("  "), Some("/v0.1.2/"), || {
            let identity = ProtocolIdentity::from_env("integration/logos-blockchain");

            assert_eq!(
                identity.protocol_name("identify/1.0.0"),
                "/logos-blockchain-v0.1.2/identify/1.0.0"
            );
        });
    }

    fn with_protocol_env(
        namespace: Option<&str>,
        protocol_type: Option<&str>,
        protocol_release: Option<&str>,
        f: impl FnOnce(),
    ) {
        let _lock = ENV_LOCK.lock().expect("env lock should not be poisoned");
        let restore = EnvRestore::capture();

        set_optional_env(PROTOCOL_NAMESPACE_VAR, namespace);
        set_optional_env(PROTOCOL_TYPE_VAR, protocol_type);
        set_optional_env(PROTOCOL_RELEASE_VAR, protocol_release);

        f();

        drop(restore);
    }

    struct EnvRestore {
        namespace: Option<String>,
        protocol_type: Option<String>,
        protocol_release: Option<String>,
    }

    impl EnvRestore {
        fn capture() -> Self {
            Self {
                namespace: std::env::var(PROTOCOL_NAMESPACE_VAR).ok(),
                protocol_type: std::env::var(PROTOCOL_TYPE_VAR).ok(),
                protocol_release: std::env::var(PROTOCOL_RELEASE_VAR).ok(),
            }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            restore_optional_env(PROTOCOL_NAMESPACE_VAR, self.namespace.as_deref());
            restore_optional_env(PROTOCOL_TYPE_VAR, self.protocol_type.as_deref());
            restore_optional_env(PROTOCOL_RELEASE_VAR, self.protocol_release.as_deref());
        }
    }

    fn set_optional_env(name: &str, value: Option<&str>) {
        match value {
            Some(value) => {
                // SAFETY: tests hold ENV_LOCK for the full mutation window, so env access here
                // is serialized.
                unsafe { std::env::set_var(name, value) }
            }
            None => {
                // SAFETY: tests hold ENV_LOCK for the full mutation window, so env access here
                // is serialized.
                unsafe { std::env::remove_var(name) }
            }
        }
    }

    fn restore_optional_env(name: &str, value: Option<&str>) {
        set_optional_env(name, value);
    }
}
