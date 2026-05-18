use std::collections::HashSet;

pub mod blend;
pub mod libp2p;
pub mod network_service;

pub const ROOT: &str = "logos_blockchain";

/// Log target namespaces follow Rust-style `module::path` segments.
const TARGET_NAMESPACE_DELIMITER: &str = "::";

#[must_use]
fn matches_target_prefix(target: &str, candidate: &str) -> bool {
    target == candidate
        || candidate
            .strip_prefix(target)
            .is_some_and(|suffix| suffix.starts_with(TARGET_NAMESPACE_DELIMITER))
}

#[must_use]
fn target_root(target: &str) -> &str {
    target
        .split(TARGET_NAMESPACE_DELIMITER)
        .next()
        .unwrap_or(target)
}

#[must_use]
pub fn all_targets() -> HashSet<&'static str> {
    std::iter::once(ROOT)
        .chain(blend::all_targets())
        .chain(libp2p::all_targets())
        .chain(network_service::all_targets())
        .collect()
}

#[must_use]
fn is_valid_logos_target_prefix(target: &str) -> bool {
    all_targets()
        .into_iter()
        .any(|known| matches_target_prefix(target, known))
}

#[must_use]
pub fn is_logos_target_root(target: &str) -> bool {
    let root = target_root(target);
    all_targets()
        .into_iter()
        .any(|known| target_root(known) == root)
}

#[must_use]
pub fn is_valid_logos_target(target: &str) -> bool {
    is_logos_target_root(target) && is_valid_logos_target_prefix(target)
}

#[cfg(test)]
mod tests {
    use super::{
        all_targets, blend, is_logos_target_root, is_valid_logos_target,
        is_valid_logos_target_prefix, libp2p, network_service,
    };

    #[test]
    fn blend_targets_are_registered() {
        assert!(blend::all_targets().contains(&blend::service::CORE));
        assert!(blend::all_targets().contains(&blend::network::core::handler::CORE_EDGE));
    }

    #[test]
    fn network_service_targets_are_registered() {
        assert!(
            network_service::all_targets().contains(&network_service::backends::libp2p::GOSSIPSUB)
        );
    }

    #[test]
    fn libp2p_targets_are_registered() {
        assert!(libp2p::all_targets().contains(&libp2p::behaviour::nat::address_mapper::ROOT));
    }

    #[test]
    fn exact_target_validation_accepts_known_targets() {
        assert!(all_targets().contains(&"logos_blockchain"));
        assert!(all_targets().contains(&blend::service::ROOT));
        assert!(all_targets().contains(&blend::service::core::KMS_POQ_GENERATOR));
        assert!(!all_targets().contains(&"logos_blockchain::blend::service::missing"));
    }

    #[test]
    fn prefix_validation_accepts_known_prefixes() {
        assert!(is_valid_logos_target_prefix("logos_blockchain"));
        assert!(is_valid_logos_target_prefix("logos_blockchain::blend"));
        assert!(is_valid_logos_target_prefix(
            "logos_blockchain::blend::service"
        ));
        assert!(is_valid_logos_target_prefix(
            "logos_blockchain::blend::network::core::core"
        ));
        assert!(!is_valid_logos_target_prefix(
            "logos_blockchain::blend::unknown"
        ));
        assert!(!is_valid_logos_target_prefix("other"));
    }

    #[test]
    fn logos_target_root_detection_matches_known_roots() {
        assert!(is_logos_target_root("logos_blockchain"));
        assert!(is_logos_target_root("logos_blockchain::blend"));
        assert!(is_logos_target_root("logos_blockchain::libp2p"));
        assert!(is_logos_target_root("logos_blockchain::network_service"));
        assert!(is_logos_target_root("logos_blockchain::blend::service"));
        assert!(is_logos_target_root(
            "logos_blockchain::blend::service::missing"
        ));
        assert!(!is_logos_target_root("bl"));
        assert!(!is_logos_target_root("libp2p"));
        assert!(!is_logos_target_root("other"));
    }

    #[test]
    fn logos_target_validation_requires_known_root_and_prefix() {
        assert!(is_valid_logos_target("logos_blockchain"));
        assert!(is_valid_logos_target("logos_blockchain::blend"));
        assert!(is_valid_logos_target("logos_blockchain::blend::service"));
        assert!(!is_valid_logos_target(
            "logos_blockchain::blend::service::missing"
        ));
        assert!(!is_valid_logos_target("libp2p"));
    }
}
