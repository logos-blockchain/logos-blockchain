//! Finding 20261104T020644-e1c105/1.
//!
//! Proof of Quota spec (proof-of-quota.md, "Construction", item 2): the Proof
//! of Leadership Quota "Ensures that the leader node would win the proof of
//! stake for **current Cryptarchia epoch** and hasn't already produced more
//! keys than the leadership quota `Q_L`". `leader_quota` is "Allowed blending
//! operations per election win", and the leader's key nullifier is derived
//! from "the winning slot of the PoL" (Step 5).
//!
//! The winning slot `pol_sl` is a private witness, and none of the public
//! inputs the verifier is given (`PublicInputs` here, the 12-signal
//! `PoQVerifierInput` underneath) carries the bounds of the current epoch. The
//! lottery ticket only binds the epoch *nonce*, so a note holder can claim any
//! slot that wins under this epoch's nonce, including slots that are not in
//! this epoch, and each such slot is a fresh "election win" worth `Q_L` new key
//! nullifiers.
//!
//! The test takes a 1%-stake leadership witness (the data of the `poq` crate's
//! own `test_leader_full_flow`), scans slots starting 2^40 slots after the
//! epoch's winning slot (no Cryptarchia epoch is anywhere near 2^40 slots
//! long), proves for the first one that wins under the same epoch nonce, and
//! checks the spec sentence: that proof must not verify against this epoch's
//! public inputs. On the code as it stands it does.

use ed25519_dalek::VerifyingKey;
use lb_groth16::{Fr, fr_from_bytes, fr_to_bytes};
use lb_pol::LotteryConstants;
use lb_poseidon2::{Digest, Poseidon2Bn254Hasher};
use lb_utils::math::NonNegativeRatio;
// The package's own library, which the rest of the workspace imports as
// `lb_blend_proofs`.
use logos_blockchain_blend_proofs::{
    quota::{
        KeyIndex, Quota, VerifiedProofOfQuota,
        inputs::prove::{
            PrivateInputs, PublicInputs,
            private::ProofOfLeadershipQuotaInputs,
            public::{CoreInputs, LeaderInputs, PowInputs},
        },
    },
    selection::derive_key_nullifier_from_secret_selection_randomness,
};
use num_bigint::BigUint;

/// Slots between the epoch's winning slot and the first slot scanned.
const SLOTS_PAST_THE_EPOCH: u64 = 1 << 40;
/// Upper bound on the scan; a 1%-stake note wins about one slot in a thousand.
const MAX_SLOTS_SCANNED: u64 = 10_000_000;

fn fr(decimal: &str) -> Fr {
    decimal.parse::<BigUint>().unwrap().into()
}

/// A domain separation tag as the circuit encodes it: the tag's bytes read as
/// a little-endian integer.
fn tag(bytes: &[u8]) -> Fr {
    fr_from_bytes(bytes).expect("a short tag is below the modulus")
}

/// The data of `lb_poq`'s `test_leader_full_flow`: a note worth 50 out of a
/// total stake of 5000, with `f = 1/10`.
fn one_percent_stake_leader() -> (PublicInputs, ProofOfLeadershipQuotaInputs) {
    let (lottery_0, lottery_1) =
        LotteryConstants::new(NonNegativeRatio::new(1, 10.try_into().unwrap()))
            .compute_lottery_values(5000);
    let public_inputs = PublicInputs {
        signing_key: VerifyingKey::from_bytes(&[1; 32]).unwrap(),
        core: CoreInputs {
            zk_root: fr(
                "12185619490528295825098195451407217973914857946530829707990590609721812577473",
            ),
            quota: Quota::new::<15>(),
        },
        leader: LeaderInputs {
            pol_ledger_aged: fr(
                "367970463510572346469727551309607212474618104803583434125220973330505217187",
            ),
            pol_epoch_nonce: fr(
                "19183275818966466009388966937999847470836267418943165691463680548408801149509",
            ),
            message_quota: Quota::new::<10>(),
            lottery_0,
            lottery_1,
        },
        pow: PowInputs {
            pow_blend_difficulty: fr(
                "2334035772366381927456001473031809359552034265892309520954243351196711606017",
            ),
            pow_quota: Quota::new::<5>(),
        },
    };
    let note = ProofOfLeadershipQuotaInputs {
        slot: 3_934_028_363,
        note_value: 50,
        transaction_hash: fr(
            "2264455058971045832756600439690776883891824153136465835848552732282836700508",
        ),
        output_number: 1108,
        aged_path_and_selectors: [
            (
                "11104092157301530528415266289001896179719940037074919849624292254162611405786",
                true,
            ),
            (
                "7132185073809921262555691477157103985074819342411989543016526500095228117354",
                false,
            ),
            (
                "11907580727147699209958498886101128744892522133797395449242728594286922858086",
                false,
            ),
            (
                "13075221440612698710788795973343285593006804290474838812157163926886238383760",
                true,
            ),
            (
                "17200451570433990100996559280535385805985882628682996186008504614174096791798",
                true,
            ),
            (
                "16750120177808191972292661243802361139298740550633002442851428788856687150475",
                false,
            ),
            (
                "12679489866011049826521697310427217461017078670664946750194685078601853495880",
                false,
            ),
            (
                "15235439481027011453071738627024275793651222671965241576356428348106464813175",
                false,
            ),
            (
                "9961832183928877694020893796261063404160642210645529511233145074735042190217",
                true,
            ),
            (
                "4326894103682035305319995256704866698441157935817295295020304417938631996729",
                true,
            ),
            (
                "17555384873204675431996467471744635783468598594303097832615135885710898785356",
                false,
            ),
            (
                "2067987409200354598518658724620929803515175794312186642703606119551864949264",
                true,
            ),
            (
                "18414033735029028807991144814771963824730052421471947864345037264843089845734",
                false,
            ),
            (
                "11883340736248057870265402327022279289678280641364875114447762692303561445604",
                false,
            ),
            (
                "7157973471780523265294561800756652242511668640260136396798977970116460727342",
                true,
            ),
            (
                "10322140357911263063410611191898227332467973200575964702516054623940247053118",
                false,
            ),
            (
                "12567824105074446475873393314739330450709906448171155888539789943621309224508",
                true,
            ),
            (
                "3778057320817011605792674467466589599030650184482297863404214181285644124757",
                true,
            ),
            (
                "15139857989447407851586537192536170660643402207011818138460800689622488846498",
                true,
            ),
            (
                "4686260554210282279381725438723974181592826312599841000574494090500374964214",
                true,
            ),
            (
                "2017001248552489998639360772220278151302627680149021505074390044394703925937",
                false,
            ),
            (
                "15644113980009580949735073673522118459196764019649835645357551831476441064371",
                true,
            ),
            (
                "5851335105157805239260310112724712629219062160101168478507890653975790109833",
                false,
            ),
            (
                "4506645166310701016292123296822379297660598150676608439399796403413830281952",
                false,
            ),
            (
                "19761070472419507198412744968226383034230108135701041508981821408866293379367",
                false,
            ),
            (
                "16473696535741964167503054455797711177314106892505373545741412745155986927894",
                true,
            ),
            (
                "7024125351521471566160638647544413645870621144999681104961251077959669126366",
                false,
            ),
            (
                "4223261943159590385933901257711007435848118520945782486495445973028111080784",
                true,
            ),
            (
                "2480120922453299704385093407865595899109107879489738668468480942431779023090",
                false,
            ),
            (
                "4872555691447955405629869337126212288862528483324109155414334863586541089650",
                false,
            ),
            (
                "2885249961213344608420596395566111778633573487967873292737118191013543595441",
                false,
            ),
            (
                "20675954944665714887729603321541315818168820606980234992582325333134286444983",
                false,
            ),
        ]
        .map(|(value, selector)| (fr(value), selector)),
        secret_key: fr(
            "14029017592631272654768426994822986470491530234220356957377918534745805708829",
        ),
    };
    (public_inputs, note)
}

/// The lottery the circuit's `would_win_leadership` evaluates, natively:
/// `ticket = zkhash(LEAD_V1, epoch_nonce, slot, note_id, sk)` must be below
/// `value * (t0 + t1 * value)`.
struct Lottery {
    epoch_nonce: Fr,
    note_id: Fr,
    secret_key: Fr,
    threshold: Fr,
}

impl Lottery {
    fn new(leader: &LeaderInputs, note: &ProofOfLeadershipQuotaInputs) -> Self {
        let public_key =
            <Poseidon2Bn254Hasher as Digest>::compress(&[tag(b"KDF"), note.secret_key]);
        let value = Fr::from(note.note_value);
        let note_id = <Poseidon2Bn254Hasher as Digest>::digest(&[
            tag(b"NOTE_ID_V1"),
            note.transaction_hash,
            Fr::from(note.output_number),
            value,
            public_key,
        ]);
        Self {
            epoch_nonce: leader.pol_epoch_nonce,
            note_id,
            secret_key: note.secret_key,
            threshold: value * (leader.lottery_0 + leader.lottery_1 * value),
        }
    }

    fn wins(&self, slot: u64) -> bool {
        let ticket = <Poseidon2Bn254Hasher as Digest>::digest(&[
            tag(b"LEAD_V1"),
            self.epoch_nonce,
            Fr::from(slot),
            self.note_id,
            self.secret_key,
        ]);
        ticket < self.threshold
    }
}

/// The key nullifier the spec's Step 5 assigns to a leader's `index`-th key
/// for `slot`.
fn leader_key_nullifier(note: &ProofOfLeadershipQuotaInputs, index: u64, slot: u64) -> Fr {
    let selection_randomness = <Poseidon2Bn254Hasher as Digest>::digest(&[
        tag(b"SELECTION_RANDOMNESS_V1"),
        note.secret_key,
        Fr::from(index),
        Fr::from(slot),
    ]);
    derive_key_nullifier_from_secret_selection_randomness(selection_randomness)
}

/// Proves a leadership `PoQ` for `note` at key index 0 and verifies it against
/// `public_inputs`, returning the key nullifier if the verifier accepted it.
///
/// The circuit refusing the witness counts as not accepted: once the slot is
/// bound to the epoch, that is where an out-of-epoch slot is stopped.
fn accepted_key_nullifier(
    public_inputs: &PublicInputs,
    note: ProofOfLeadershipQuotaInputs,
) -> Option<Fr> {
    let (proof, _) = VerifiedProofOfQuota::new(
        public_inputs,
        PrivateInputs::new_proof_of_leadership_quota_inputs(KeyIndex::new::<0>(), note),
    )
    .ok()?;
    let key_nullifier = proof.key_nullifier();
    proof
        .into_inner()
        .verify(public_inputs)
        .ok()
        .map(|_| key_nullifier)
}

#[test]
fn leadership_quota_proof_only_accepts_a_slot_of_the_current_epoch() {
    let (public_inputs, epoch_note) = one_percent_stake_leader();
    let epoch_slot = epoch_note.slot;
    let lottery = Lottery::new(&public_inputs.leader, &epoch_note);
    assert!(
        lottery.wins(epoch_slot),
        "the native lottery must agree with the circuit on the epoch's own winning slot"
    );
    // Positive control: the epoch's own winning slot proves and verifies, so a
    // rejection below cannot come from a broken pipeline.
    assert_eq!(
        accepted_key_nullifier(&public_inputs, epoch_note.clone()),
        Some(leader_key_nullifier(&epoch_note, 0, epoch_slot)),
        "the epoch's own winning slot must yield a valid leadership proof"
    );

    let first_scanned = epoch_slot + SLOTS_PAST_THE_EPOCH;
    let out_of_epoch_slot = (first_scanned..first_scanned + MAX_SLOTS_SCANNED)
        .find(|slot| lottery.wins(*slot))
        .expect("a 1%-stake note wins about one slot in a thousand");
    let mut out_of_epoch_note = epoch_note.clone();
    out_of_epoch_note.slot = out_of_epoch_slot;

    let out_of_epoch_nullifier = accepted_key_nullifier(&public_inputs, out_of_epoch_note);

    assert!(
        out_of_epoch_nullifier.is_none(),
        "spec: PoQ_L ensures the leader would win the proof of stake for the *current* \
         Cryptarchia epoch, yet a leadership proof for slot {out_of_epoch_slot} \
         ({} slots after this epoch's winning slot {epoch_slot}, found after scanning {} slots) \
         verified against this epoch's public inputs. Its key nullifier {} is not among the \
         epoch slot's Q_L nullifiers (index 0 there is {}), so every such slot mints Q_L extra \
         keys.",
        out_of_epoch_slot - epoch_slot,
        out_of_epoch_slot - first_scanned + 1,
        out_of_epoch_nullifier.map_or_else(String::new, |nullifier| hex::encode(fr_to_bytes(
            &nullifier
        ))),
        hex::encode(fr_to_bytes(&leader_key_nullifier(&epoch_note, 0, epoch_slot))),
    );
}
