//! The Blend admission puzzle backing the proof of work branch of the Proof of
//! Quota.
//!
//! The branch admits a prover that holds neither stake nor an SDP declaration:
//! its admission right is a puzzle solution, a private nonce whose ticket falls
//! below the epoch's Blend difficulty. Spec: <https://lip.logos.co/blockchain/raw/proof-of-quota.html#constraints>.
//!
//! The derivation here mirrors the circuit's, so that a solution found outside
//! the circuit is one the circuit accepts.

use core::num::NonZeroU64;
use std::sync::LazyLock;

use lb_groth16::{AdditiveGroup as _, Field as _, Fr, fr_from_bytes};

use crate::{ZkHash, ZkHashExt as _, quota::inputs::prove::private::ProofOfWorkQuotaInputs};

/// `d_blend`: the threshold a puzzle ticket must be strictly below for a
/// solution to be admitted.
///
/// A smaller threshold admits a smaller fraction of tickets, so a *smaller*
/// value is a *harder* puzzle.
pub type PowTarget = Fr;

const DOMAIN_SEPARATION_TAG: [u8; 12] = *b"BLEND_POW_V1";
static DOMAIN_SEPARATION_TAG_FR: LazyLock<ZkHash> = LazyLock::new(|| {
    fr_from_bytes(&DOMAIN_SEPARATION_TAG[..])
        .expect("DST for the Blend PoW ticket calculation must be correct.")
});

/// Derives the puzzle ticket of a candidate nonce, exactly as the circuit
/// does: `zkhash(BLEND_POW_V1, pol_epoch_nonce, pow_nonce)`.
///
/// The puzzle takes no block reference. The circuit cannot establish that a
/// value is the hash of a canonical block, so the only time-dependent input is
/// the epoch nonce, and a solution is bound to an epoch and to nothing finer.
#[must_use]
pub fn derive_pow_ticket(epoch_nonce: ZkHash, pow_nonce: ZkHash) -> ZkHash {
    [*DOMAIN_SEPARATION_TAG_FR, epoch_nonce, pow_nonce].hash()
}

/// Whether `ticket` satisfies `difficulty`.
///
/// A field has no order, so this is not field arithmetic: the comparison is
/// over the two values' canonical integer representatives in `[0, p-1]`, which
/// is what [`Fr`]'s [`Ord`] compares.
#[must_use]
pub fn is_winning_ticket(ticket: ZkHash, difficulty: PowTarget) -> bool {
    ticket < difficulty
}

/// Searches for a nonce whose ticket satisfies `difficulty`, trying `attempts`
/// candidates from `starting_nonce` onwards.
///
/// Returns [`None`] when the budget is exhausted without a hit, so the caller
/// keeps control of how long a single search runs and can hand its thread back
/// to the runtime between rounds. A zero `difficulty` admits no ticket at all,
/// so it returns immediately rather than spending the budget on a search that
/// cannot succeed.
///
/// `starting_nonce` must be sampled with full entropy. The nonce stands in the
/// secret key position of the key nullifier derivation, so two provers starting
/// from the same nonce derive colliding nullifiers and the network discards one
/// of their messages as a duplicate.
#[must_use]
pub fn solve_puzzle(
    epoch_nonce: ZkHash,
    difficulty: PowTarget,
    starting_nonce: ZkHash,
    attempts: NonZeroU64,
) -> Option<ProofOfWorkQuotaInputs> {
    if difficulty == PowTarget::ZERO {
        return None;
    }

    let mut pow_nonce = starting_nonce;
    for _ in 0..attempts.get() {
        if is_winning_ticket(derive_pow_ticket(epoch_nonce, pow_nonce), difficulty) {
            return Some(ProofOfWorkQuotaInputs { pow_nonce });
        }
        pow_nonce += ZkHash::ONE;
    }
    None
}

#[cfg(test)]
mod tests {
    use const_hex::FromHex as _;
    use lb_groth16::{AdditiveGroup as _, Field as _, fr_from_bytes_unchecked, fr_to_bytes};
    use num_bigint::BigUint;

    use crate::quota::{
        ED25519_PUBLIC_KEY_SIZE, Ed25519PublicKey, Quota,
        fixtures::valid_proof_of_work_quota_inputs,
        inputs::prove::{PublicInputs, private::ProofOfWorkQuotaInputs},
        pow::{
            DOMAIN_SEPARATION_TAG_FR, PowTarget, derive_pow_ticket, is_winning_ticket, solve_puzzle,
        },
    };

    /// The largest field element, `p - 1`, as an integer.
    fn largest_target() -> BigUint {
        BigUint::from_bytes_le(&fr_to_bytes(&(-PowTarget::ONE)))
    }

    #[test]
    fn pow_ticket_dst_encoding() {
        // Blend spec: <https://lip.logos.co/blockchain/raw/proof-of-quota.html>
        assert_eq!(
            *DOMAIN_SEPARATION_TAG_FR,
            fr_from_bytes_unchecked(&<[u8; 12]>::from_hex("0x424c454e445f504f575f5631").unwrap()),
        );
    }

    /// The fixture is a solution the circuit accepts, so the ticket this module
    /// derives for it must be below the fixture's difficulty. A derivation that
    /// disagreed with the circuit's would produce an unrelated field element,
    /// which the fixture's threshold admits only by chance.
    #[test]
    fn fixture_solution_satisfies_its_difficulty() {
        let (PublicInputs { leader, pow, .. }, ProofOfWorkQuotaInputs { pow_nonce }) =
            valid_proof_of_work_quota_inputs(
                Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE]).unwrap(),
                Quota::ONE,
            );

        assert!(is_winning_ticket(
            derive_pow_ticket(leader.pol_epoch_nonce, pow_nonce),
            pow.pow_blend_difficulty
        ));
    }

    #[test]
    fn solved_puzzle_satisfies_its_difficulty() {
        let epoch_nonce = BigUint::from(42u64).into();
        // Roughly one candidate in a thousand wins, so the search is short but
        // a wrong ticket derivation is very unlikely to pass it.
        let difficulty: PowTarget = (largest_target() >> 10u32).into();

        let ProofOfWorkQuotaInputs { pow_nonce } = solve_puzzle(
            epoch_nonce,
            difficulty,
            BigUint::from(1u64).into(),
            1_000_000.try_into().unwrap(),
        )
        .unwrap();

        assert!(is_winning_ticket(
            derive_pow_ticket(epoch_nonce, pow_nonce),
            difficulty
        ));
    }

    #[test]
    fn every_ticket_satisfies_the_largest_difficulty() {
        let epoch_nonce = BigUint::from(42u64).into();
        let difficulty: PowTarget = largest_target().into();

        // The first candidate tried is a solution, whatever it is.
        let starting_nonce = BigUint::from(7u64).into();
        assert_eq!(
            solve_puzzle(
                epoch_nonce,
                difficulty,
                starting_nonce,
                1.try_into().unwrap()
            )
            .map(|inputs| inputs.pow_nonce),
            Some(starting_nonce)
        );
    }

    #[test]
    fn unsatisfiable_difficulty_is_not_searched() {
        assert!(
            solve_puzzle(
                BigUint::from(42u64).into(),
                PowTarget::ZERO,
                BigUint::from(1u64).into(),
                u64::MAX.try_into().unwrap(),
            )
            .is_none()
        );
    }

    #[test]
    fn exhausted_budget_yields_no_solution() {
        // The hardest satisfiable difficulty: only the zero ticket wins, which
        // a handful of candidates will not find.
        assert!(
            solve_puzzle(
                BigUint::from(42u64).into(),
                BigUint::from(1u64).into(),
                BigUint::from(1u64).into(),
                10.try_into().unwrap(),
            )
            .is_none()
        );
    }
}
