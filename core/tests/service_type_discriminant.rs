// This integration target links core as a normal dependency, so `cfg(test)` is
// not enabled for `logos_blockchain_core` itself.
use logos_blockchain_core::sdp::ServiceType;

#[test]
fn test_service_discriminant_is_rejected_for_dependency_builds() {
    assert_eq!(ServiceType::try_from(0), Ok(ServiceType::BlendNetwork));
    assert_eq!(ServiceType::try_from(1), Err(()));
}
