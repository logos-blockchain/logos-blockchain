use serde_yaml::{Mapping, Value};
use testing_framework_core::scenario::DynError;

const SDP_DECLARE_OPCODE: u64 = 0x20;

pub(super) fn from_logos_yaml(source: &str) -> Result<String, DynError> {
    let source: Value = serde_yaml::from_str(source)?;
    let genesis = prepare_genesis(&source["cryptarchia"]["genesis_block"])?;

    let mut output = project(&source, &["blend"])?;
    output["network"] = project(
        &source["network"],
        &[
            "kademlia_protocol_name",
            "identify_protocol_name",
            "chain_sync_protocol_name",
        ],
    )?;
    output["time"] = project(&source["time"], &["slot_duration"])?;
    output["mempool"] = project(&source["mempool"], &["pubsub_topic"])?;
    if !output["mempool"]["pubsub_topic"]
        .as_str()
        .is_some_and(|topic| topic.starts_with('/'))
    {
        return Err("Nimbos preparation requires a shared mempool topic starting with '/'".into());
    }

    let mut cryptarchia = project(
        &source["cryptarchia"],
        &[
            "epoch_config",
            "security_param",
            "slot_activation_coeff",
            "learning_rate",
            "sdp_config",
            "gossipsub_protocol",
            "faucet_pk",
        ],
    )?;
    if cryptarchia["faucet_pk"].is_null() {
        return Err("Nimbos preparation cannot represent an absent faucet key without changing stake exclusion".into());
    }

    prepare_min_stake(&mut cryptarchia["sdp_config"])?;
    cryptarchia["genesis_block"] = genesis;
    output["cryptarchia"] = cryptarchia;

    Ok(serde_yaml::to_string(&output)?)
}

fn prepare_min_stake(sdp_config: &mut Value) -> Result<(), DynError> {
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

    rename_required_field(min_stake, "timestamp", "epoch")
}

fn prepare_genesis(source: &Value) -> Result<Value, DynError> {
    if source["header"].get("body_root").is_some()
        || source
            .get("uncle_headers")
            .is_some_and(|uncles| !uncles.as_sequence().is_some_and(Vec::is_empty))
    {
        return Err(
            "Nimbos reconstructs genesis using a transaction root; it cannot preserve the body_root/uncle commitment algorithm"
                .into(),
        );
    }
    if source["header"].get("block_root").is_none() {
        return Err(
            "Nimbos preparation requires a genesis with a transaction-root commitment".into(),
        );
    }
    let transactions = source["transactions"]
        .as_sequence()
        .ok_or("Nimbos preparation requires exactly one genesis transaction")?;
    let [transaction] = transactions.as_slice() else {
        return Err("Nimbos preparation requires exactly one genesis transaction".into());
    };

    let mut genesis = project(source, &["header", "signature"])?;
    genesis["transactions"] = vec![prepare_genesis_transaction(transaction)?].into();
    Ok(genesis)
}

fn prepare_genesis_transaction(source: &Value) -> Result<Value, DynError> {
    let mut transaction = source.clone();
    let mantle_tx = transaction
        .get_mut("mantle_tx")
        .ok_or("Nimbos preparation requires a genesis mantle transaction")?;
    let ops = mantle_tx
        .get_mut("ops")
        .and_then(Value::as_sequence_mut)
        .ok_or("Nimbos preparation requires genesis transaction operations")?;

    for op in ops {
        if op["opcode"].as_u64() == Some(SDP_DECLARE_OPCODE) {
            prepare_sdp_declaration(op)?;
        }
    }

    Ok(transaction)
}

fn prepare_sdp_declaration(op: &mut Value) -> Result<(), DynError> {
    let payload = op
        .get_mut("payload")
        .and_then(Value::as_mapping_mut)
        .ok_or("Nimbos preparation requires an SDP declaration payload")?;

    rename_required_field(payload, "service_note_id", "locked_note_id")
}

fn project(source: &Value, fields: &[&str]) -> Result<Value, DynError> {
    let mut output = Mapping::new();
    for field in fields {
        let value = source
            .get(*field)
            .ok_or_else(|| format!("Nimbos preparation is missing '{field}'"))?;
        output.insert((*field).into(), value.clone());
    }
    Ok(Value::Mapping(output))
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
