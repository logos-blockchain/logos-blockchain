use std::iter::repeat;

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
    let ops = &genesis_tx.mantle_tx().ops;
    assert!(
        ops.len() <= count,
        "genesis_tx contains {} declarations more than the requested number of configs: {count}",
        ops.len()
    );

    ops.iter()
        .filter_map(|op| match op {
            Op::SDPDeclare(decl) => Some(GeneralSdpConfig {
                declaration_id: Some(decl.id()),
            }),
            _ => None,
        })
        .chain(repeat(GeneralSdpConfig {
            declaration_id: None,
        }))
        .take(count)
        .collect()
}
