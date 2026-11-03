use ark_bn254::{Bn254, Fq, Fq2, Fr, G2Affine};
use ark_ff::AdditiveGroup as _;
use ark_groth16::{Groth16, ProvingKey, r1cs_to_qap::LibsnarkReduction};
use ark_relations::{
    lc,
    r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError},
};
use rand_chacha::{ChaCha20Rng, rand_core::SeedableRng as _};

use crate::{Groth16Proof, Groth16VerificationKey};

// Three public inputs and one constraint: (x0 + x1 + x2)^2 = square.
pub struct ThreeInputCircuit {
    pub inputs: [Fr; 3],
}

impl ConstraintSynthesizer<Fr> for ThreeInputCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        let [x0, x1, x2] = self.inputs;
        let sum = x0 + x1 + x2;
        let x0 = cs.new_input_variable(|| Ok(x0))?;
        let x1 = cs.new_input_variable(|| Ok(x1))?;
        let x2 = cs.new_input_variable(|| Ok(x2))?;
        let square = cs.new_witness_variable(|| Ok(sum * sum))?;
        cs.enforce_constraint(lc!() + x0 + x1 + x2, lc!() + x0 + x1 + x2, lc!() + square)
    }
}

#[must_use]
pub fn three_input_setup(seed: u64) -> (Groth16VerificationKey, ProvingKey<Bn254>) {
    let proving_key =
        Groth16::<Bn254, LibsnarkReduction>::generate_random_parameters_with_reduction(
            ThreeInputCircuit {
                inputs: [Fr::ZERO; 3],
            },
            &mut ChaCha20Rng::seed_from_u64(seed),
        )
        .expect("the three input circuit has a setup");
    let key = Groth16VerificationKey {
        alpha_1: proving_key.vk.alpha_g1,
        beta_2: proving_key.vk.beta_g2,
        gamma_2: proving_key.vk.gamma_g2,
        delta_2: proving_key.vk.delta_g2,
        ic: proving_key.vk.gamma_abc_g1.clone(),
    };
    (key, proving_key)
}

#[must_use]
pub fn three_input_prove(
    proving_key: &ProvingKey<Bn254>,
    inputs: [Fr; 3],
    seed: u64,
) -> Groth16Proof {
    let proof = Groth16::<Bn254, LibsnarkReduction>::create_random_proof_with_reduction(
        ThreeInputCircuit { inputs },
        proving_key,
        &mut ChaCha20Rng::seed_from_u64(seed),
    )
    .expect("the three input circuit proves");
    Groth16Proof {
        pi_a: proof.a,
        pi_b: proof.b,
        pi_c: proof.c,
    }
}

// A point on the twist outside the prime order subgroup.
pub fn off_subgroup_g2() -> G2Affine {
    (1u64..)
        .find_map(|n| {
            G2Affine::get_point_from_x_unchecked(Fq2::new(Fq::from(n), Fq::ZERO), false)
                .filter(|point| !point.is_in_correct_subgroup_assuming_on_curve())
        })
        .expect("the twist has points outside the subgroup")
}
