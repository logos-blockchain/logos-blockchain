use std::marker::PhantomData;
use std::str::FromStr as _;

use ark_bn254::Fr;
use groth16::fr_from_bytes;
use key_management_system_keys::keys::UnsecuredZkKey;
use nomos_core::{crypto::ZkHasher, mantle::ops::leader_claim::VoucherCm};
use overwatch::services::state::{ServiceState, StateUpdater};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct LeaderState<Settings> where Settings: Clone {
    pub voucher_sk: Vec<UnsecuredZkKey>,
    _phantom: PhantomData<Settings>
}

impl<Settings: Clone> ServiceState for LeaderState<Settings> {
    type Settings = Settings;
    type Error = ();

    fn from_settings(_settings: &Self::Settings) -> Result<Self, Self::Error> {
        Ok(Self {
            voucher_sk: Vec::default(),
            _phantom: PhantomData
        })
    }
}

impl<Settings: Clone> LeaderState<Settings> {
    pub fn add_new_voucher_sk(
        &mut self,
        leader_sk: UnsecuredZkKey,
        state_updater: &StateUpdater<Option<Self>>,
    ) -> VoucherCm {
        let mut seed = [0u8; 31];
        OsRng.fill_bytes(&mut seed);
        let mut hash = ZkHasher::new();
        hash.compress(&[*leader_sk.as_fr(), fr_from_bytes(&seed).unwrap()]);
        let voucher_secret = hash.finalize();
        self.voucher_sk
            .push(UnsecuredZkKey::new(voucher_secret.clone()));
        state_updater.update(Some(*self));
        let mut hash = ZkHasher::new();
        hash.compress(&[
            Fr::from_str("1668646695034522932676805048878418").unwrap(),
            voucher_secret,
        ]);

        VoucherCm::from(hash.finalize())
    }
}

