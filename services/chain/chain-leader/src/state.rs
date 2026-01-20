use std::{convert::Infallible, marker::PhantomData, str::FromStr as _};

use ark_bn254::Fr;
use lb_core::{crypto::ZkHasher, mantle::ops::leader_claim::VoucherCm};
use lb_groth16::fr_from_bytes;
use lb_key_management_system_keys::keys::UnsecuredZkKey;
use overwatch::services::state::{ServiceState, StateUpdater};
use rand::{RngCore as _, rngs::OsRng};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct LeaderState<Settings> {
    pub voucher_sk: Vec<UnsecuredZkKey>,
    _phantom: PhantomData<Settings>,
}

impl<Settings> ServiceState for LeaderState<Settings> {
    type Settings = Settings;
    type Error = Infallible;

    fn from_settings(_settings: &Self::Settings) -> Result<Self, Self::Error> {
        Ok(Self {
            voucher_sk: Vec::default(),
            _phantom: PhantomData,
        })
    }
}

impl<Settings: Clone> LeaderState<Settings> {
    pub fn add_new_voucher_sk(
        &mut self,
        leader_sk: &UnsecuredZkKey,
        state_updater: &StateUpdater<Option<Self>>,
    ) -> VoucherCm {
        let mut seed = [0u8; 31];
        OsRng.fill_bytes(&mut seed);
        let mut hash = ZkHasher::new();
        hash.compress(&[*leader_sk.as_fr(), fr_from_bytes(&seed).unwrap()]);
        let voucher_secret = hash.finalize();
        self.voucher_sk.push(UnsecuredZkKey::new(voucher_secret));
        state_updater.update(Some(self.clone()));
        let mut hash = ZkHasher::new();
        hash.compress(&[
            Fr::from_str("1668646695034522932676805048878418").unwrap(),
            voucher_secret,
        ]);

        VoucherCm::from(hash.finalize())
    }
}
