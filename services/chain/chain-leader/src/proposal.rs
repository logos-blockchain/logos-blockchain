use lb_core::{
    block::{Block, BlockTransactions, UncleHeaders, body_root},
    header::{HeaderId, block_id, pol_msg},
    mantle::{
        Value,
        notes::NoteCommitment,
        traits::{Hashable, StorageSize},
        transactions::hash::TxHash,
    },
    proofs::leader_proof::{Groth16LeaderProof, LeaderPrivate, LeaderProof, LeaderPublic},
};
use lb_cryptarchia_engine::Slot;
use lb_groth16::{AdditiveGroup as _, Fr};

use crate::Error;

// The proof of a block whose body is not fixed yet, so it cannot be proven: it
// only lets the proposer apply the header before selecting the transactions,
// which it runs before the reward of the block is known.
pub struct ProposalHeaderProof;

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

    fn reward_commitment(&self) -> NoteCommitment {
        NoteCommitment(Fr::ZERO)
    }

    fn reward_value(&self) -> Value {
        0
    }
}

// The PoL msg of the header the block gets: its block ID, over its body root.
#[must_use]
pub fn block_msg<Tx: Hashable<Hash = TxHash>>(
    parent: &HeaderId,
    slot: Slot,
    uncles: &UncleHeaders,
    txs: &[Tx],
    reward_commitment: &NoteCommitment,
    reward_value: Value,
    entropy_contribution: &Fr,
) -> Fr {
    pol_msg(&block_id(
        parent,
        slot,
        &body_root(uncles, txs),
        reward_commitment,
        reward_value,
        entropy_contribution,
    ))
}

// Proves the PoL bound to the block and builds the block; blocking.
pub fn assemble_block<Tx>(
    parent: HeaderId,
    slot: Slot,
    uncles: UncleHeaders,
    txs: BlockTransactions<Tx>,
    private: LeaderPrivate,
) -> Result<Block<Tx>, Error>
where
    Tx: Hashable<Hash = TxHash> + StorageSize,
{
    let proof = Groth16LeaderProof::prove(private)?;
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
    use lb_key_management_system_service::keys::{UnsecuredZkKey, ZkPublicKey};
    use lb_ledger::LedgerState;

    use super::*;
    use crate::leadership::public_inputs_for_slot;

    #[test]
    fn proposed_block_proof_is_bound_to_its_body_and_reward() {
        let config = crate::leadership::test_config();
        let key = UnsecuredZkKey::new(Fr::from(7u64));
        let nullifier_key = key.nullifier_key();
        let note = notes::Note::new(1_000, Fr::from(11u64), key.to_public_key());
        let ledger = LedgerState::from_notes([note], &config, 0);
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
            .find(|(_, public)| check_winning(&note, *public, nullifier_key, nullifier_key))
            .expect("a single staker wins within 1000 slots");
        let mut wallet = pol_wallet_inputs(
            &note,
            LEDGER_SET,
            &derive_note_nullifier(&note.commitment(), &nullifier_key),
            &aged_path,
            &epoch_state.note_sets,
            ledger.latest_note_sets(),
        )
        .unwrap();
        wallet.nullifier_key = nullifier_key;
        wallet.staking_secret_key = nullifier_key;
        let reward = notes::Note::new(
            0x6666,
            Fr::from(0x5555u64),
            ZkPublicKey::new(Fr::from(0x4444u64)),
        );
        wallet.reward_public_key = *reward.public_key.as_fr();
        wallet.reward_nonce = reward.nonce;

        let parent = HeaderId::from([1; 32]);
        let txs = BlockTransactions::<SignedOps<Preverified, StandardMode>>::empty();
        let unbound = LeaderPrivate::new(public, wallet.clone(), Fr::ZERO, reward.value);
        let msg = block_msg(
            &parent,
            slot,
            &UncleHeaders::empty(),
            txs.as_slice(),
            &reward.commitment(),
            reward.value,
            &unbound.entropy_contribution(),
        );
        let private = unbound.with_msg(msg);
        let block = assemble_block(parent, slot, UncleHeaders::empty(), txs, private).unwrap();

        assert!(block.header().verify(&public));
        let proof = block.header().leader_proof().clone();
        assert!(!Header::new(parent, ContentId::from([9; 32]), slot, proof).verify(&public));
        // `Groth16LeaderProof::from_parts` is crate private in core: `msg`, bound to
        // the reward note, is proven under another key of that note.
        wallet.reward_public_key = Fr::from(5u64);
        let other_key = assemble_block(
            parent,
            slot,
            UncleHeaders::empty(),
            BlockTransactions::<SignedOps<Preverified, StandardMode>>::empty(),
            LeaderPrivate::new(public, wallet, msg, reward.value),
        )
        .unwrap();
        assert!(!other_key.header().verify(&public));
    }
}
