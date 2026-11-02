use lb_core::{
    block::{Block, BlockTransactions, UncleHeaders, body_root},
    header::{HeaderId, pol_msg},
    mantle::{
        traits::{Hashable, StorageSize},
        transactions::hash::TxHash,
    },
    proofs::leader_proof::{Groth16LeaderProof, LeaderPrivate, LeaderProof, LeaderPublic},
};
use lb_cryptarchia_engine::Slot;
use lb_groth16::{AdditiveGroup as _, Fr};
use lb_key_management_system_service::keys::ZkPublicKey;

use crate::Error;

// The proof of a block whose body is not fixed yet, so it cannot be proven: it
// only lets the proposer apply the header before selecting the transactions.
pub struct ProposalHeaderProof {
    pub reward_key: ZkPublicKey,
}

impl LeaderProof for ProposalHeaderProof {
    fn verify(&self, _public_inputs: &LeaderPublic) -> bool {
        true
    }

    fn verify_genesis(&self) -> bool {
        false
    }

    fn entropy(&self) -> Fr {
        Fr::ZERO
    }

    fn reward_key(&self) -> &ZkPublicKey {
        &self.reward_key
    }
}

// The PoL msg of the header the block gets: pol_msg over its body root.
#[must_use]
pub fn block_msg<Tx: Hashable<Hash = TxHash>>(
    parent: &HeaderId,
    slot: Slot,
    uncles: &UncleHeaders,
    txs: &[Tx],
    reward_key: &ZkPublicKey,
) -> Fr {
    pol_msg(parent, slot, &body_root(uncles, txs), reward_key)
}

// Proves the PoL bound to the block and builds the block; blocking.
pub fn assemble_block<Tx>(
    parent: HeaderId,
    slot: Slot,
    uncles: UncleHeaders,
    txs: BlockTransactions<Tx>,
    private: LeaderPrivate,
    reward_key: ZkPublicKey,
) -> Result<Block<Tx>, Error>
where
    Tx: Hashable<Hash = TxHash> + StorageSize,
{
    let proof = Groth16LeaderProof::prove(private, reward_key)?;
    Ok(Block::create(parent, slot, uncles, proof, txs)?)
}

#[cfg(test)]
mod tests {
    use lb_core::{
        header::{ContentId, Header},
        mantle::{
            SignedOps,
            ledger::verification_mode::StandardMode,
            note_set::{CommitmentMmr, LEDGER_SET},
            notes::{self, derive_note_nullifier},
            transactions::states::Preverified,
        },
        proofs::leader_proof::{check_winning, pol_wallet_inputs},
    };
    use lb_key_management_system_service::keys::UnsecuredZkKey;
    use lb_ledger::{ConsensusExcludedKeys, LedgerState};

    use super::*;
    use crate::leadership::public_inputs_for_slot;

    #[test]
    fn proposed_block_proof_is_bound_to_its_body_and_reward_key() {
        let config = crate::leadership::test_config();
        let secret_key = Fr::from(7u64);
        let public_key = UnsecuredZkKey::new(secret_key).to_public_key();
        let note = notes::Note::new(1_000, Fr::from(11u64), public_key);
        let ledger = LedgerState::from_notes([note], &config, 0, &ConsensusExcludedKeys::empty());
        let (_, aged_path) = CommitmentMmr::new()
            .push_with_paths(note.commitment(), &mut [])
            .unwrap();
        let epoch_state = ledger.epoch_state();
        let (slot, public) = (1..1_000u64)
            .map(|slot| {
                (
                    Slot::from(slot),
                    public_inputs_for_slot(
                        epoch_state,
                        Slot::from(slot),
                        ledger.latest_note_sets(),
                    ),
                )
            })
            .find(|(_, public)| check_winning(&note, *public, &public_key, secret_key))
            .expect("a single staker wins within 1000 slots");
        let mut wallet = pol_wallet_inputs(
            &note,
            LEDGER_SET,
            &derive_note_nullifier(&note.commitment(), &secret_key),
            &aged_path,
            &epoch_state.note_sets,
            ledger.latest_note_sets(),
        )
        .unwrap();
        wallet.secret_key = secret_key;

        let parent = HeaderId::from([1; 32]);
        let reward_key = ZkPublicKey::new(Fr::from(0x4444u64));
        let txs = BlockTransactions::<SignedOps<Preverified, StandardMode>>::empty();
        let msg = block_msg(
            &parent,
            slot,
            &UncleHeaders::empty(),
            txs.as_slice(),
            &reward_key,
        );
        let private = LeaderPrivate::new(public, wallet, msg);
        let block = assemble_block(
            parent,
            slot,
            UncleHeaders::empty(),
            txs,
            private.clone(),
            reward_key,
        )
        .unwrap();

        assert!(block.header().verify(&public));
        let proof = block.header().leader_proof().clone();
        assert!(!Header::new(parent, ContentId::from([9; 32]), slot, proof).verify(&public));
        // `Groth16LeaderProof::from_parts` is crate private in core: the same
        // witness, bound to `reward_key`, is proven under another reward key.
        let other_key = assemble_block(
            parent,
            slot,
            UncleHeaders::empty(),
            BlockTransactions::<SignedOps<Preverified, StandardMode>>::empty(),
            private,
            ZkPublicKey::new(Fr::from(5u64)),
        )
        .unwrap();
        assert!(!other_key.header().verify(&public));
    }
}
