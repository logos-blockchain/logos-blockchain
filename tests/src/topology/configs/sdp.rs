use lb_core::{
    mantle::{GenesisTx as _, Op, genesis_tx::GenesisTx},
    sdp::DeclarationId,
};

#[derive(Clone)]
pub struct GeneralSdpConfig {
    pub declaration_id: Option<DeclarationId>,
}

#[must_use]
pub fn create_sdp_configs(genesis_tx: &GenesisTx) -> Vec<GeneralSdpConfig> {
    genesis_tx
        .mantle_tx()
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::SDPDeclare(decl) => Some(GeneralSdpConfig {
                declaration_id: Some(decl.id()),
            }),
            _ => None,
        })
        .collect()
}
