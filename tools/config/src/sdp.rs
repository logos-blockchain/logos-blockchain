use std::iter::repeat_n;

use lb_core::{
    mantle::{GenesisTx as _, Op, genesis_tx::GenesisTx},
    sdp::DeclarationId,
};

#[derive(Clone)]
pub struct GeneralSdpConfig {
    pub declaration_id: Option<DeclarationId>,
}

#[must_use]
pub fn create_sdp_configs(genesis_tx: &GenesisTx, count: usize) -> Vec<GeneralSdpConfig> {
    let mut configs = genesis_tx
        .mantle_tx()
        .ops()
        .iter()
        .filter_map(|op| match op {
            Op::SDPDeclare(decl) => Some(GeneralSdpConfig {
                declaration_id: Some(decl.id()),
            }),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(
        configs.len() <= count,
        "genesis_tx contains {} declarations more than the requested number of configs: {count}",
        configs.len()
    );

    configs.extend(repeat_n(
        GeneralSdpConfig {
            declaration_id: None,
        },
        count - configs.len(),
    ));
    assert_eq!(configs.len(), count);
    configs
}
