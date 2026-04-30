use std::collections::HashSet;

pub mod blend;

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
    blend::all_targets().into_iter().collect()
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
        is_valid_logos_target_prefix,
    };

    #[test]
    fn blend_targets_are_registered() {
        assert!(blend::all_targets().contains(&blend::service::CORE));
        assert!(blend::all_targets().contains(&blend::network::core::handler::CORE_EDGE));
    }

    #[test]
    fn exact_target_validation_accepts_known_targets() {
        assert!(all_targets().contains(&blend::service::ROOT));
        assert!(all_targets().contains(&blend::service::core::KMS_POQ_GENERATOR));
        assert!(!all_targets().contains(&"blend::service::missing"));
    }

    #[test]
    fn prefix_validation_accepts_known_prefixes() {
        assert!(is_valid_logos_target_prefix("blend"));
        assert!(is_valid_logos_target_prefix("blend::service"));
        assert!(is_valid_logos_target_prefix("blend::network::core::core"));
        assert!(!is_valid_logos_target_prefix("blend::unknown"));
        assert!(!is_valid_logos_target_prefix("other"));
    }

    #[test]
    fn logos_target_root_detection_matches_known_roots() {
        assert!(is_logos_target_root("blend"));
        assert!(is_logos_target_root("blend::service"));
        assert!(is_logos_target_root("blend::service::missing"));
        assert!(!is_logos_target_root("bl"));
        assert!(!is_logos_target_root("libp2p"));
        assert!(!is_logos_target_root("other"));
    }

    #[test]
    fn logos_target_validation_requires_known_root_and_prefix() {
        assert!(is_valid_logos_target("blend"));
        assert!(is_valid_logos_target("blend::service"));
        assert!(!is_valid_logos_target("blend::service::missing"));
        assert!(!is_valid_logos_target("libp2p"));
    }
}
