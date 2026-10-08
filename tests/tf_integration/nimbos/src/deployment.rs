use serde_yaml::{Mapping, Value};
use testing_framework_core::scenario::DynError;

const SDP_DECLARE_OPCODE: u64 = 0x20;

/// The supported Logos and Nimbos deployment YAML layouts are very similar.
/// Only two field names need translating here; all other data is preserved.
pub fn from_logos_yaml(source: &str) -> Result<String, DynError> {
    let mut output: Value = serde_yaml::from_str(source)?;
    let cryptarchia = output
        .get_mut("cryptarchia")
        .ok_or("Nimbos preparation requires cryptarchia settings")?;

    let sdp_config = cryptarchia
        .get_mut("sdp_config")
        .ok_or("Nimbos preparation requires SDP settings")?;

    let min_stake = sdp_config
        .get_mut("min_stake")
        .and_then(Value::as_mapping_mut)
        .ok_or("Nimbos preparation requires a min_stake mapping")?;

    // Block activation and epoch activation coincide only at genesis (zero).
    if min_stake
        .get("timestamp")
        .is_some_and(|timestamp| timestamp.as_u64() != Some(0))
    {
        return Err("Nimbos preparation cannot translate a nonzero minimum-stake block activation into an epoch".into());
    }

    rename_required_field(min_stake, "timestamp", "epoch")?;

    let transactions = cryptarchia
        .get_mut("genesis_block")
        .and_then(|genesis| genesis.get_mut("transactions"))
        .and_then(Value::as_sequence_mut)
        .ok_or("Nimbos preparation requires genesis transactions")?;

    for transaction in transactions {
        let mantle_tx = transaction
            .get_mut("mantle_tx")
            .ok_or("Nimbos preparation requires a genesis mantle transaction")?;
        let ops = mantle_tx
            .get_mut("ops")
            .and_then(Value::as_sequence_mut)
            .ok_or("Nimbos preparation requires genesis transaction operations")?;

        for op in ops {
            if op["opcode"].as_u64() != Some(SDP_DECLARE_OPCODE) {
                continue;
            }

            let payload = op
                .get_mut("payload")
                .and_then(Value::as_mapping_mut)
                .ok_or("Nimbos preparation requires an SDP declaration payload")?;

            rename_required_field(payload, "service_note_id", "locked_note_id")?;
        }
    }

    // Nimbos validates its native schema; preserve everything we do not translate.
    Ok(serde_yaml::to_string(&output)?)
}

fn rename_required_field(fields: &mut Mapping, from: &str, to: &str) -> Result<(), DynError> {
    if fields.contains_key(from) && fields.contains_key(to) {
        return Err(format!("Nimbos preparation received both {from} and {to}").into());
    }

    if let Some(value) = fields.remove(from) {
        fields.insert(to.into(), value);
    }
    if !fields.contains_key(to) {
        return Err(format!("Nimbos preparation requires '{from}' or '{to}'").into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_yaml::Value;

    use super::from_logos_yaml;

    // Structural fixture for conversion, not a runnable genesis. Cryptographic
    // values are opaque here: the adapter must preserve them, not recompute them.
    const LOGOS_INPUT: &str = "
# Settings outside the translated fields must also survive conversion.
additional_settings: { value: preserved }
blend:
  common: { protocol_name: /blend/test, minimum_network_size: 2 }
network:
  kademlia_protocol_name: /kad/test
  identify_protocol_name: /identify/test
  chain_sync_protocol_name: /chain-sync/test
time:
  slot_duration: { secs: 2, nanos: 0 }
mempool:
  pubsub_topic: /mempool/test
cryptarchia:
  epoch_config:
    epoch_stake_distribution_stabilization: 3
    epoch_period_nonce_buffer: 3
    epoch_period_nonce_stabilization: 4
  security_param: 10
  slot_activation_coeff: { numerator: 1, denominator: 10 }
  learning_rate: 0.1
  sdp_config:
    service_params:
      BN: { inactivity_period: 10, epoch: 0 }
    min_stake: { threshold: 100, timestamp: 0 }
  gossipsub_protocol: /cryptarchia/test
  faucet_pk: faucet-key
  genesis_block:
    header:
      version: 1
      parent_block: parent-hash
      slot: 0
      body_root: body-hash
      proof_of_leadership:
        proof: leadership-proof
        entropy_contribution: entropy
        leader_key: leader-key
        voucher_cm: voucher
    signature: genesis-signature
    uncle_headers: []
    transactions:
      - mantle_tx:
          ops:
            - opcode: 0
              payload: { value: untouched }
            - opcode: 32
              payload:
                service_note_id: service-note
                provider_id: provider-key
          ledger_tx: { outputs: [funded-note] }
        ops_proofs: [declaration-proof]
        ledger_tx_proof: ledger-proof
";

    #[test]
    fn preserves_network_and_genesis_data_with_native_field_names() {
        let converted: Value =
            serde_yaml::from_str(&from_logos_yaml(LOGOS_INPUT).unwrap()).unwrap();
        let expected_yaml = LOGOS_INPUT
            .replace("timestamp: 0", "epoch: 0")
            .replace("service_note_id:", "locked_note_id:");
        let expected: Value = serde_yaml::from_str(&expected_yaml).unwrap();

        assert_eq!(converted, expected);
    }
}
