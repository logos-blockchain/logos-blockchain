//! The uncle validity rules

use std::collections::HashSet;

use lb_core::{
    block::{Block, HeaderError, SignedHeader, UncleHeaders},
    header::HeaderId,
};
use lb_cryptarchia_engine::Branch;

use crate::{Cryptarchia, Error};

#[expect(
    clippy::multiple_inherent_impl,
    reason = "grouping the uncle validity rules separately from the main impl"
)]
impl Cryptarchia {
    /// Verifies every uncle carried by the block against the chain the block
    /// extends.
    ///
    /// # Rules
    /// - Each uncle's slot must be older than the block's slot.
    /// - Each uncle must not be on the chain that the block extends.
    /// - Each uncle's parent must be on the chain the block extends, within the
    ///   uncle reference window.
    /// - Each uncle's header signature and `PoL` must be valid.
    pub(crate) fn verify_uncles<Tx>(&self, block: &Block<Tx>) -> Result<(), Error> {
        if block.uncle_headers().is_empty() {
            return Ok(());
        }

        let header = block.header();
        let slot = header.slot();
        let parent = self
            .consensus
            .branches()
            .get(&header.parent())
            .ok_or_else(|| Error::ParentMissing {
                parent: header.parent(),
                info: Box::new(self.info()),
            })?;

        // Each uncle's slot must be older than the block's slot.
        for uncle in block.uncle_headers().iter() {
            if uncle.header().slot() >= slot {
                return Err(Error::InvalidUncle {
                    uncle: uncle.header().id(),
                    reason: UncleError::NotStrictlyOlder,
                });
            }
        }

        // Each uncle's parent must be on the chain the block extends, within the
        // uncle reference window.
        let uncle_reference_window = self
            .ledger
            .config()
            .consensus_config
            .uncle_reference_window()
            .get();
        let window_start = slot.into_inner().saturating_sub(uncle_reference_window);
        self.verify_uncles_ancestry(block.uncle_headers(), parent, window_start)?;

        // Each uncle's header (including its signature) must be valid,
        // and its `PoL` must be valid.
        for uncle in block.uncle_headers().iter() {
            uncle
                .verify()
                .map_err(UncleError::from)
                .and_then(|()| self.verify_uncle_pol(uncle))
                .map_err(|reason| Error::InvalidUncle {
                    uncle: uncle.header().id(),
                    reason,
                })?;
        }
        Ok(())
    }

    /// Verifies the following rules:
    /// - Each uncle must not be on the chain that the block extends.
    /// - Each uncle's parent must be on the chain the block extends, within the
    ///   uncle reference window.
    fn verify_uncles_ancestry(
        &self,
        uncle_headers: &UncleHeaders,
        parent: &Branch<HeaderId>,
        window_start: u64,
    ) -> Result<(), Error> {
        let uncles: HashSet<_> = uncle_headers.ids().collect();
        let uncle_parents: HashSet<_> = uncle_headers.parents().collect();

        // Walk back the chain from the block's parent to the window boundary,
        // collecting the uncle parents that are found during the walk-back.
        let mut found_uncle_parents = HashSet::new();
        let mut current = Some(parent);
        while let Some(block) = current {
            if block.slot().into_inner() < window_start {
                break;
            }
            if uncles.contains(&block.id()) {
                return Err(Error::InvalidUncle {
                    uncle: block.id(),
                    reason: UncleError::OnChain,
                });
            }
            if uncle_parents.contains(&block.id()) {
                found_uncle_parents.insert(block.id());
            }
            if block.parent() == block.id() {
                break; // Reached the oldest block in the tree.
            }
            current = self.consensus.branches().get(&block.parent());
        }

        // Return an error if any uncle's parent was not found during the walk-back.
        for uncle in uncle_headers.iter() {
            if !found_uncle_parents.contains(&uncle.header().parent()) {
                return Err(Error::InvalidUncle {
                    uncle: uncle.header().id(),
                    reason: UncleError::ParentNotOnChain,
                });
            }
        }
        Ok(())
    }

    /// Verifies the leadership proof carried by an uncle.
    fn verify_uncle_pol(&self, uncle: &SignedHeader) -> Result<(), UncleError> {
        // The proof of leadership must verify against the ledger state of the
        // uncle's parent, which must exist since the parent is on the chain.
        let parent_state = self
            .ledger
            .state(&uncle.header().parent())
            .expect("ledger state of a block on the chain must exist");
        parent_state
            .verify_proof_of_leadership::<_, HeaderId>(
                uncle.header().slot(),
                uncle.header().leader_proof(),
                self.ledger.config(),
            )
            .map_err(|_| UncleError::InvalidProof)
    }
}

/// Why an uncle carried by a block fails the uncle validity rules,
/// making the block itself invalid.
#[derive(Debug, thiserror::Error)]
pub enum UncleError {
    #[error("not strictly older than the block")]
    NotStrictlyOlder,
    #[error("parent not on the chain that the block is extending, within the window")]
    ParentNotOnChain,
    #[error("on the chain that the block is extending")]
    OnChain,
    #[error("at a slot no uncle can be proposed for")]
    InvalidSlot,
    #[error("invalid header signature")]
    InvalidSignature,
    #[error("invalid proof of leadership")]
    InvalidProof,
}

impl From<HeaderError> for UncleError {
    fn from(error: HeaderError) -> Self {
        match error {
            HeaderError::GenesisSlot => Self::InvalidSlot,
            HeaderError::Signature => Self::InvalidSignature,
        }
    }
}

#[cfg(test)]
mod tests {
    use lb_core::{
        block::BlockTransactions,
        header::{ContentId, Header},
        mantle::{SignedMantleTx, Utxo, transactions::states::Preverified},
        proofs::leader_proof::Groth16LeaderProof,
    };
    use lb_cryptarchia_engine::{Slot, UncleSlots};
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};
    use lb_ledger::LedgerState;
    use rand::thread_rng;

    use super::*;
    use crate::tests::{ledger_config, try_build_block, utxo};

    #[test]
    fn test_accept_valid_uncle() {
        let (mut cryptarchia, _, u1, _, zk_key, utxo) = chain_with_fork();

        let (b2, _) = try_build_block(
            &cryptarchia,
            cryptarchia.tip(),
            utxo,
            &zk_key,
            u1.header().slot().strict_add(1.into()),
            UncleHeaders::new([signed_header(&u1)]),
        )
        .unwrap();

        cryptarchia
            .try_apply_block(&b2, b2.header().slot())
            .expect("a block referencing a valid uncle should be applied");
        assert_eq!(
            cryptarchia
                .consensus
                .branches()
                .get(&b2.header().id())
                .unwrap()
                .uncle_slots(),
            &[u1.header().slot()].into()
        );
    }

    #[test]
    fn test_reject_uncle_not_strictly_older() {
        let (mut cryptarchia, _, u1, u1_key, ..) = chain_with_fork();

        // The block is at the same slot as the uncle it references.
        let block = craft_block_with_uncles(
            cryptarchia.tip(),
            u1.header().slot(),
            UncleHeaders::new([signed_header(&u1)]),
            u1.header().leader_proof(),
            &u1_key,
        );

        let Err(err) = cryptarchia.try_apply_block(&block, block.header().slot()) else {
            panic!("expected the block to be rejected");
        };
        assert!(matches!(
            err,
            Error::InvalidUncle {
                reason: UncleError::NotStrictlyOlder,
                ..
            }
        ));
    }

    #[test]
    fn test_reject_uncle_whose_parent_is_outside_window() {
        let (mut cryptarchia, _, u1, u1_key, ..) = chain_with_fork();

        // The block is more than `w_u` slots after the uncle's parent,
        // which puts the uncle's parent outside the window.
        let uncle_reference_window = cryptarchia
            .ledger
            .config()
            .consensus_config
            .uncle_reference_window()
            .get();
        let block = craft_block_with_uncles(
            cryptarchia.tip(),
            u1.header()
                .slot()
                .strict_add((uncle_reference_window + 1).into()),
            UncleHeaders::new([signed_header(&u1)]),
            u1.header().leader_proof(),
            &u1_key,
        );

        let Err(err) = cryptarchia.try_apply_block(&block, block.header().slot()) else {
            panic!("expected the block to be rejected");
        };
        assert!(matches!(
            err,
            Error::InvalidUncle {
                reason: UncleError::ParentNotOnChain,
                ..
            }
        ));
    }

    #[test]
    fn test_reject_uncle_on_the_chain() {
        let (mut cryptarchia, b1, u1, u1_key, ..) = chain_with_fork();

        // The block references its own parent `B1` as an uncle.
        let block = craft_block_with_uncles(
            cryptarchia.tip(),
            b1.header().slot().strict_add(1.into()),
            UncleHeaders::new([signed_header(&b1)]),
            u1.header().leader_proof(),
            &u1_key,
        );

        let Err(err) = cryptarchia.try_apply_block(&block, block.header().slot()) else {
            panic!("expected the block to be rejected");
        };
        assert!(matches!(
            err,
            Error::InvalidUncle {
                reason: UncleError::OnChain,
                ..
            }
        ));
    }

    #[test]
    fn reject_uncle_whose_parent_is_not_on_the_chain() {
        let (mut cryptarchia, _, u1, u1_key, ..) = chain_with_fork();

        // An uncle whose parent is unknown to the chain.
        let header = Header::new(
            HeaderId::from([9u8; 32]),
            ContentId::from([0u8; 32]),
            u1.header().slot(),
            u1.header().leader_proof().clone(),
        );
        let signature = header.sign(&u1_key).unwrap();
        let block = craft_block_with_uncles(
            cryptarchia.tip(),
            u1.header().slot().strict_add(1.into()),
            UncleHeaders::new([SignedHeader::new(header, signature)]),
            u1.header().leader_proof(),
            &u1_key,
        );

        let Err(err) = cryptarchia.try_apply_block(&block, block.header().slot()) else {
            panic!("expected the block to be rejected");
        };
        assert!(matches!(
            err,
            Error::InvalidUncle {
                reason: UncleError::ParentNotOnChain,
                ..
            }
        ));
    }

    #[test]
    fn reject_uncle_with_invalid_signature() {
        let (mut cryptarchia, _, u1, u1_key, ..) = chain_with_fork();

        // The uncle's header is intact, but signed by a key that is not its
        // leader.
        let signature = u1
            .header()
            .sign(&Ed25519Key::generate(&mut thread_rng()))
            .unwrap();
        let block = craft_block_with_uncles(
            cryptarchia.tip(),
            u1.header().slot().strict_add(1.into()),
            UncleHeaders::new([SignedHeader::new(u1.header().clone(), signature)]),
            u1.header().leader_proof(),
            &u1_key,
        );

        let Err(err) = cryptarchia.try_apply_block(&block, block.header().slot()) else {
            panic!("expected the block to be rejected");
        };
        assert!(matches!(
            err,
            Error::InvalidUncle {
                reason: UncleError::InvalidSignature,
                ..
            }
        ));
    }

    #[test]
    fn reject_uncle_with_invalid_pol() {
        let (mut cryptarchia, _, u1, u1_key, ..) = chain_with_fork();

        // The uncle carries `U1`'s proof at a different slot, against which
        // the proof was not proven. The signature itself is valid.
        let wrong_uncle_slot = u1.header().slot().strict_add(1.into());
        let header = Header::new(
            HeaderId::from([0u8; 32]),
            ContentId::from([0u8; 32]),
            wrong_uncle_slot,
            u1.header().leader_proof().clone(),
        );
        let signature = header.sign(&u1_key).unwrap();
        let block = craft_block_with_uncles(
            cryptarchia.tip(),
            u1.header().slot().strict_add(2.into()),
            UncleHeaders::new([SignedHeader::new(header, signature)]),
            u1.header().leader_proof(),
            &u1_key,
        );

        let Err(err) = cryptarchia.try_apply_block(&block, block.header().slot()) else {
            panic!("expected the block to be rejected");
        };
        assert!(matches!(
            err,
            Error::InvalidUncle {
                reason: UncleError::InvalidProof,
                ..
            }
        ));
    }

    /// A chain `G --- B1` with a fork block `U1` also extending `G`, at the
    /// same slot as `B1`.
    #[expect(clippy::type_complexity, reason = "a test helper")]
    fn chain_with_fork() -> (
        Cryptarchia,
        Block<SignedMantleTx<Preverified>>,
        Block<SignedMantleTx<Preverified>>,
        Ed25519Key,
        ZkKey,
        Utxo,
    ) {
        let config = ledger_config(3.try_into().unwrap());
        let genesis_id = [0; 32].into();
        let (zk_key, utxo) = utxo();
        let mut cryptarchia = Cryptarchia::from_lib(
            genesis_id,
            LedgerState::from_utxos([utxo], &config),
            genesis_id,
            config,
            lb_cryptarchia_engine::State::Bootstrapping,
            Slot::genesis(),
            0,
            UncleSlots::default(),
        );

        // Both extend the genesis, and the same key wins the same slot, so the
        // two blocks differ only in their (randomly generated) block leaders.
        let (u1, u1_key) = try_build_block(
            &cryptarchia,
            genesis_id,
            utxo,
            &zk_key,
            Slot::new(1),
            UncleHeaders::empty(),
        )
        .unwrap();
        let (b1, _) = try_build_block(
            &cryptarchia,
            genesis_id,
            utxo,
            &zk_key,
            Slot::new(1),
            UncleHeaders::empty(),
        )
        .unwrap();
        cryptarchia
            .try_apply_block(&b1, b1.header().slot())
            .unwrap();

        (cryptarchia, b1, u1, u1_key, zk_key, utxo)
    }

    fn signed_header(block: &Block<SignedMantleTx<Preverified>>) -> SignedHeader {
        SignedHeader::new(block.header().clone(), *block.signature())
    }

    /// Crafts a block carrying the uncles, without a winning `PoL` for `slot`,
    /// which is fine because uncles are verified before the block's own proof.
    fn craft_block_with_uncles(
        parent: HeaderId,
        slot: Slot,
        uncle_headers: UncleHeaders,
        proof: &Groth16LeaderProof,
        key: &Ed25519Key,
    ) -> Block<SignedMantleTx<Preverified>> {
        Block::create(
            parent,
            slot,
            uncle_headers,
            proof.clone(),
            BlockTransactions::empty(),
            key,
        )
        .unwrap()
    }
}
