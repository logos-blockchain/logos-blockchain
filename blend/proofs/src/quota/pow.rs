//! The Blend admission puzzle backing the proof of work branch of the Proof of
//! Quota.
//!
//! The branch admits a prover that holds neither stake nor an SDP declaration:
//! its admission right is a puzzle solution, a private nonce whose ticket falls
//! below the epoch's Blend difficulty.
//!
//! Spec: the branch is added to the Proof of Quota by
//! <https://github.com/logos-co/logos-lips/pull/400> (revisions 1.2.0 and
//! 1.3.0 of `proof-of-quota.md`), which is the authority for everything in this
//! module until it lands. The published page at
//! <https://lip.logos.co/blockchain/raw/proof-of-quota.html> still describes
//! the two-branch construction and does not define the `PoW` target, quota,
//! nonce or ticket derivation.
//!
//! The circuit, in contrast, already implements the branch: circuits v0.5.6
//! carries the `pow_nonce` witness the derivation below is written against, and
//! [`crate::quota::fixtures::valid_proof_of_work_quota_inputs`] is a solution
//! taken from that circuit's own tests. The tests here check this module's
//! ticket against that fixture, so the derivation is pinned to the circuit
//! rather than only to prose.

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

/// The value a candidate nonce derives, which a solution must place below the
/// epoch's [`PowTarget`] to be admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowTicket(ZkHash);

impl PowTicket {
    #[must_use]
    pub fn derive(epoch_nonce: ZkHash, pow_nonce: ZkHash) -> Self {
        Self([*DOMAIN_SEPARATION_TAG_FR, epoch_nonce, pow_nonce].hash())
    }

    #[must_use]
    pub fn satisfies(self, difficulty: PowTarget) -> bool {
        self.0 < difficulty
    }
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
        PowTicket::derive(epoch_nonce, pow_nonce)
            .satisfies(difficulty)
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
    use const_hex::FromHex as _;
    use lb_groth16::{Field as _, fr_from_bytes_unchecked};
    use num_bigint::BigUint;
    use rand::rngs::OsRng;

    use crate::quota::{
        ED25519_PUBLIC_KEY_SIZE, Ed25519PublicKey, Quota,
        fixtures::valid_proof_of_work_quota_inputs,
        inputs::prove::{PublicInputs, private::ProofOfWorkQuotaInputs},
        pow::{DOMAIN_SEPARATION_TAG_FR, PowTarget, PowTicket, solve_puzzle},
    };

    /// The largest field element, `p - 1`, as an integer.
    fn largest_target() -> BigUint {
        (-PowTarget::ONE).into()
    }

    #[test]
    fn pow_ticket_dst_encoding() {
        // Blend spec: <https://github.com/logos-co/logos-lips/pull/400>
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

        assert!(
            PowTicket::derive(leader.pol_epoch_nonce, pow_nonce)
                .satisfies(pow.pow_blend_difficulty)
        );
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

        assert!(PowTicket::derive(epoch_nonce, pow_nonce).satisfies(difficulty));
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
