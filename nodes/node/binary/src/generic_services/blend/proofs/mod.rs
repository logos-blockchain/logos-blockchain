use cfg_if::cfg_if;

#[cfg(feature = "pol-dev-mode")]
mod pol_dev_mode;

cfg_if! {
    if #[cfg(feature = "pol-dev-mode")] {
        pub type CoreProofsGenerator<CorePoQGenerator> = pol_dev_mode::MockedCoreProofsGenerator<CorePoQGenerator>;
        pub type EdgeProofsGenerator = pol_dev_mode::MockedEdgeProofsGenerator;
        pub type ProofsVerifier = pol_dev_mode::MockedBlendProofsVerifier;
    } else {
        pub type CoreProofsGenerator<CorePoQGenerator> = lb_blend::scheduling::message_blend::provers::core_and_leader::RealCoreAndLeaderProofsGenerator<CorePoQGenerator>;
        pub type EdgeProofsGenerator = lb_blend::scheduling::message_blend::provers::leader::RealLeaderProofsGenerator;
        pub type ProofsVerifier = lb_blend::message::crypto::proofs::RealProofsVerifier;
    }
}
