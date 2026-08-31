#[cfg(test)]
mod wallet_name_tests {
    use super::{NodeWalletKey, NodeWalletKeyRole, node_wallet_name};

    fn node_wallet_key(role: NodeWalletKeyRole) -> NodeWalletKey {
        NodeWalletKey {
            wallet_pk: "00".repeat(32),
            role,
        }
    }

    #[test]
    fn node_wallet_names_use_semantic_roles_before_numbered_fallbacks() {
        let mut generic_key_index = 0;

        assert_eq!(
            node_wallet_name(
                "NODE_1",
                &node_wallet_key(NodeWalletKeyRole::Funding),
                &mut generic_key_index
            ),
            "NODE_1_WALLET_FUNDING"
        );
        assert_eq!(
            node_wallet_name(
                "NODE_1",
                &node_wallet_key(NodeWalletKeyRole::VoucherMaster),
                &mut generic_key_index
            ),
            "NODE_1_WALLET_VOUCHER_MASTER"
        );
        assert_eq!(
            node_wallet_name(
                "NODE_1",
                &node_wallet_key(NodeWalletKeyRole::BlendZk),
                &mut generic_key_index
            ),
            "NODE_1_WALLET_BLEND_ZK"
        );
        assert_eq!(
            node_wallet_name(
                "NODE_1",
                &node_wallet_key(NodeWalletKeyRole::General),
                &mut generic_key_index
            ),
            "NODE_1_WALLET_GENERAL_1"
        );
        assert_eq!(
            node_wallet_name(
                "NODE_1",
                &node_wallet_key(NodeWalletKeyRole::General),
                &mut generic_key_index
            ),
            "NODE_1_WALLET_GENERAL_2"
        );
    }

    #[test]
    fn semantic_wallet_names_do_not_consume_general_indexes() {
        let mut generic_key_index = 0;

        assert_eq!(
            node_wallet_name(
                "NODE_1",
                &node_wallet_key(NodeWalletKeyRole::Funding),
                &mut generic_key_index
            ),
            "NODE_1_WALLET_FUNDING"
        );
        assert_eq!(generic_key_index, 0);
    }
}

#[cfg(test)]
mod scenario_wallet_key_tests {
    use lb_key_management_system_service::keys::{Ed25519Key, secured_key::SecuredKey as _};

    use super::*;

    #[test]
    fn removes_only_external_scenario_wallet_keys() {
        let mut kms_keys = HashMap::new();
        let mut known_keys = HashMap::new();
        let scenario_accounts = [
            WalletAccount::deterministic(100, 0, true).expect("account"),
            WalletAccount::deterministic(101, 0, true).expect("account"),
        ];
        let sponsored_fee_account =
            WalletAccount::deterministic(103, 0, true).expect("fee account");
        let scenario_key_ids = scenario_accounts
            .iter()
            .map(wallet_account_key_id)
            .chain(std::iter::once(wallet_account_key_id(
                &sponsored_fee_account,
            )))
            .collect::<HashSet<_>>();

        for account in scenario_accounts
            .iter()
            .chain(std::iter::once(&sponsored_fee_account))
        {
            let key: Key = account.secret_key.clone().into();
            let key_id = key_id_for_preload_backend(&key);
            kms_keys.insert(key_id.clone(), key);
            known_keys.insert(key_id, account.secret_key.as_public_key());
        }

        let unrelated = WalletAccount::deterministic(102, 0, true).expect("account");
        let unrelated_key: Key = unrelated.secret_key.clone().into();
        let unrelated_key_id = key_id_for_preload_backend(&unrelated_key);
        kms_keys.insert(unrelated_key_id.clone(), unrelated_key);
        known_keys.insert(
            unrelated_key_id.clone(),
            unrelated.secret_key.as_public_key(),
        );

        let ed25519_key: Key = Ed25519Key::from_bytes(&[9; 32]).into();
        let ed25519_key_id = key_id_for_preload_backend(&ed25519_key);
        kms_keys.insert(ed25519_key_id.clone(), ed25519_key);
        let voucher_master = WalletAccount::deterministic(104, 0, true).expect("account");
        let voucher_key: Key = voucher_master.secret_key.clone().into();
        let voucher_master_key_id = key_id_for_preload_backend(&voucher_key);
        kms_keys.insert(voucher_master_key_id.clone(), voucher_key);
        known_keys.insert(
            voucher_master_key_id.clone(),
            voucher_master.secret_key.as_public_key(),
        );

        remove_external_scenario_wallet_keys_from_maps(
            &mut known_keys,
            &mut kms_keys,
            &scenario_key_ids,
        );
        remove_external_scenario_wallet_keys_from_maps(
            &mut known_keys,
            &mut kms_keys,
            &scenario_key_ids,
        );

        for key_id in &scenario_key_ids {
            assert!(!kms_keys.contains_key(key_id));
            assert!(!known_keys.contains_key(key_id));
        }
        assert!(kms_keys.contains_key(&unrelated_key_id));
        assert!(known_keys.contains_key(&unrelated_key_id));
        assert!(kms_keys.contains_key(&ed25519_key_id));
        assert!(kms_keys.contains_key(&voucher_master_key_id));
        assert!(known_keys.contains_key(&voucher_master_key_id));
    }

    #[test]
    fn empty_scenario_wallet_set_changes_nothing() {
        let mut kms_keys = HashMap::new();
        let mut known_keys = HashMap::new();
        let account = WalletAccount::deterministic(105, 0, true).expect("account");
        let key: Key = account.secret_key.clone().into();
        let key_id = key_id_for_preload_backend(&key);
        kms_keys.insert(key_id.clone(), key);
        known_keys.insert(key_id, account.secret_key.as_public_key());
        let before_kms = kms_keys.clone();
        let before_known_keys = known_keys.clone();

        remove_external_scenario_wallet_keys_from_maps(
            &mut known_keys,
            &mut kms_keys,
            &HashSet::new(),
        );

        assert_eq!(kms_keys, before_kms);
        assert_eq!(known_keys, before_known_keys);
    }
}
use super::{
    lifecycle::{
        node_wallet_name, remove_external_scenario_wallet_keys_from_maps, wallet_account_key_id,
    },
    *,
};
