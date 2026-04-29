#![forbid(unsafe_code)]

pub mod blend;

#[must_use]
pub(crate) fn matches_target_prefix(target: &str, candidate: &str) -> bool {
    target == candidate
        || candidate
            .strip_prefix(target)
            .is_some_and(|suffix| suffix.starts_with("::"))
}

#[must_use]
fn target_root(target: &str) -> &str {
    target.split("::").next().unwrap_or(target)
}

#[must_use]
pub fn all_targets() -> Vec<&'static str> {
    blend::all_targets()
}

#[must_use]
pub fn is_valid_target(target: &str) -> bool {
    all_targets().into_iter().any(|known| known == target)
}

#[must_use]
pub fn is_valid_logos_target_prefix(target: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::{blend, is_logos_target_root, is_valid_logos_target_prefix, is_valid_target};

    #[test]
    fn blend_targets_are_registered() {
        assert!(blend::all_targets().contains(&blend::service::CORE));
        assert!(blend::all_targets().contains(&blend::network::core::handler::CORE_EDGE));
    }

    #[test]
    fn exact_target_validation_accepts_known_targets() {
        assert!(is_valid_target(blend::service::ROOT));
        assert!(is_valid_target(blend::service::core::KMS_POQ_GENERATOR));
        assert!(!is_valid_target("blend::service::missing"));
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
}
