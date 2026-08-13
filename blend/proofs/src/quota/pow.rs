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

use lb_groth16::{AdditiveGroup as _, Fr, fr_from_bytes};
use rand::RngCore;

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
/// candidates drawn from `rng`.
///
/// Returns [`None`] when the budget is exhausted without a hit, so the caller
/// keeps control of how long a single search runs and can hand its thread back
/// to the runtime between rounds. A zero `difficulty` admits no ticket at all,
/// so it returns immediately rather than spending the budget on a search that
/// cannot succeed.
#[must_use]
pub fn solve_puzzle<Rng>(
    epoch_nonce: ZkHash,
    difficulty: PowTarget,
    rng: &mut Rng,
    attempts: NonZeroU64,
) -> Option<ProofOfWorkQuotaInputs>
where
    Rng: RngCore + ?Sized,
{
    if difficulty == PowTarget::ZERO {
        return None;
    }

    (0..attempts.get()).find_map(|_| {
        let pow_nonce = random_nonce(rng);
        is_winning_ticket(derive_pow_ticket(epoch_nonce, pow_nonce), difficulty)
            .then_some(ProofOfWorkQuotaInputs { pow_nonce })
    })
}

/// A field element sampled uniformly from `[0, p-1]`.
///
/// Reducing 256 random bits modulo `p` would not be uniform: `2^256` is `5p`
/// plus a remainder, so the residues inside that remainder have six preimages
/// against the other five and would come up a fifth more often. Resampling
/// instead of reducing removes the bias, and masking the draw first is what
/// makes that cheap: `p` lies between `2^253` and `2^254`, so a full-width draw
/// lands below `p` only 19% of the time — over five draws per nonce — whereas
/// clearing the two bits above `2^254` excludes no value of the field and
/// raises that to 76%, about a third of an extra draw. This runs once per
/// mining candidate, so the difference is the search's throughput.
fn random_nonce<Rng>(rng: &mut Rng) -> ZkHash
where
    Rng: RngCore + ?Sized,
{
    /// Clears the two bits above the 254 the modulus fits in.
    const TOP_BYTE_MASK: u8 = 0b0011_1111;

    loop {
        let mut bytes = [0u8; size_of::<ZkHash>()];
        rng.fill_bytes(&mut bytes);
        *bytes
            .last_mut()
            .expect("A nonce is wider than a single byte.") &= TOP_BYTE_MASK;
        // `fr_from_bytes` rejects anything at or above the modulus, which is
        // exactly the rejection this sampling needs.
        if let Ok(nonce) = fr_from_bytes(&bytes) {
            return nonce;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use const_hex::FromHex as _;
    use lb_groth16::{AdditiveGroup as _, Field as _, fr_from_bytes_unchecked, fr_to_bytes};
    use num_bigint::BigUint;
    use rand::rngs::OsRng;

    use crate::quota::{
        ED25519_PUBLIC_KEY_SIZE, Ed25519PublicKey, Quota,
        fixtures::valid_proof_of_work_quota_inputs,
        inputs::prove::{PublicInputs, private::ProofOfWorkQuotaInputs},
        pow::{
            DOMAIN_SEPARATION_TAG_FR, PowTarget, derive_pow_ticket, is_winning_ticket,
            random_nonce, solve_puzzle,
        },
    };

    /// The largest field element, `p - 1`, as an integer.
    fn largest_target() -> BigUint {
        (-PowTarget::ONE).into()
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
            &mut OsRng,
            1_000_000.try_into().unwrap(),
        )
        .unwrap();

        assert!(is_winning_ticket(
            derive_pow_ticket(epoch_nonce, pow_nonce),
            difficulty
        ));
    }

    #[test]
    fn exhausted_budget_yields_no_solution() {
        // The hardest satisfiable difficulty: only the zero ticket wins, which
        // a handful of candidates will not find.
        assert!(
            solve_puzzle(
                BigUint::from(42u64).into(),
                BigUint::from(1u64).into(),
                &mut OsRng,
                10.try_into().unwrap(),
            )
            .is_none()
        );
    }
}
