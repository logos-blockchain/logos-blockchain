use std::iter::repeat_n;

use lb_core::mantle::{ops::OpRef, traits::MantleTx as _, transactions::GenesisTx};
use lb_sdp_service::DeclarationConfig;

#[derive(Clone)]
pub struct GeneralSdpConfig {
    pub declaration: Option<DeclarationConfig>,
}

#[must_use]
pub fn create_sdp_configs(genesis_tx: &GenesisTx, count: usize) -> Vec<GeneralSdpConfig> {
    let mut configs = genesis_tx
        .op_refs()
        .into_iter()
        .filter_map(|op| match op {
            OpRef::SDPDeclare(declaration) => Some(GeneralSdpConfig {
                declaration: Some(declaration.into()),
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
        GeneralSdpConfig { declaration: None },
        count - configs.len(),
    ));
    assert_eq!(configs.len(), count);
    configs
}
