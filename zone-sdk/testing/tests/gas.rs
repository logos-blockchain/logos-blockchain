//! The simulated node charges real gas: funded transactions pay what the
//! ledger prices them at, unfunded ones are evicted, and a price rise is paid.

use common::{Network, Posted, World, inscription, passive, seq_key};
use lb_core::mantle::{
    Op, OpProof, OpRef, SignedOps,
    ops::channel::inscribe::InscriptionOp,
    traits::Hashable as _,
    transactions::{GasPrices, Ops, tx_list::OpProofs},
};
use lb_zone_sdk::node_types::TxHash;
use logos_blockchain_zone_sdk_testing::{SimNode, world as common};

/// Fee the included `hash` paid: transfer inputs minus outputs, read from the
/// ledger before its block.
fn fee_paid(node: &SimNode, hash: &TxHash) -> (u64, u64) {
    let block = node
        .block(node.block_of_tx(hash).expect("included"))
        .expect("block");
    let before = node.block(block.parent).expect("parent").ledger;
    let tx = block
        .txs
        .iter()
        .find(|tx| tx.hash() == *hash)
        .expect("tx in block");
    let mut inputs = 0u64;
    let mut outputs = 0u64;
    for op in tx.op_refs_iter() {
        if let OpRef::Transfer(transfer) = op {
            inputs += transfer
                .inputs
                .iter()
                .map(|id| before.note(id).expect("input existed").value)
                .sum::<u64>();
            outputs += transfer.outputs.amount().expect("amount");
        }
    }
    let priced = before.gas_cost(tx).expect("priced").into_inner();
    (inputs - outputs, priced)
}

#[tokio::test(start_paused = true)]
async fn funded_inscription_pays_its_gas_cost() {
    let mut world = World::new(Network::Linear);
    let a = world.start("SEQ_A", passive()).await;
    let hash = world.publish_included(a, "priced").await;
    let (paid, priced) = fee_paid(world.node(), &hash);
    assert!(priced > 1, "real gas cost expected, got {priced}");
    assert!(paid >= priced, "paid {paid} below the priced {priced}");
    // The SDK reserves a priority percentage over the mandatory fee.
    let reserve = priced * lb_zone_sdk::sequencer::FundingConfig::DEFAULT_PRIORITY_FEE_PERCENT;
    assert!(
        paid <= priced + reserve.div_ceil(100),
        "paid {paid} exceeds the priced {priced} plus its reserve"
    );
}

#[tokio::test(start_paused = true)]
async fn unfunded_inscription_is_evicted() {
    let mut world = World::new(Network::Linear);
    let a = world.start("SEQ_A", passive()).await;
    let first = world.publish_included(a, "funded").await;
    world.finalize_tx(first).await;

    let key = seq_key("SEQ_A");
    let channel_id = world.channel_id();
    let tip = world
        .node()
        .channel_state_at_tip(channel_id)
        .expect("channel")
        .tip_message;
    let op = InscriptionOp {
        channel_id,
        inscription: inscription("unfunded"),
        parent: tip,
        signer: key.public_key().into_unverified(),
    };
    let ops = Ops::from([Op::ChannelInscribe(op)]);
    let signature = key.sign_payload(ops.hash().as_signing_bytes());
    let tx = SignedOps::from_parts(ops, OpProofs::from([OpProof::Ed25519Sig(signature)]))
        .expect("signed");
    let hash = tx.hash();
    world.node().submit(tx);
    world.steps(2).await;

    assert!(!world.node().is_included(&hash), "unfunded tx was mined");
    let rejection = world
        .node()
        .rejected()
        .into_iter()
        .find(|r| r.tx_hash == hash)
        .expect("rejected");
    assert!(
        rejection.reason.contains("insufficient balance"),
        "{}",
        rejection.reason
    );
    assert!(
        world.node().mempool().is_empty(),
        "evicted from the mempool"
    );
}

#[tokio::test(start_paused = true)]
async fn price_rise_is_paid_by_the_next_publish() {
    let mut world = World::new(Network::Linear);
    let a = world.start("SEQ_A", passive()).await;
    let cheap = world.publish_included(a, "cheap").await;
    // Prices take effect with the next block, like a storage-market update.
    world.node().set_gas_prices(GasPrices::new(5, 3));
    world.steps(1).await;
    let dear = world.publish_included(a, "dear").await;

    let (paid_cheap, priced_cheap) = fee_paid(world.node(), &cheap);
    let (paid_dear, priced_dear) = fee_paid(world.node(), &dear);
    assert!(
        priced_dear > priced_cheap * 2,
        "{priced_dear} vs {priced_cheap}"
    );
    assert!(
        paid_dear >= priced_dear,
        "paid {paid_dear} below {priced_dear}"
    );
    assert!(paid_dear > paid_cheap, "{paid_dear} vs {paid_cheap}");
    assert!(
        world.node().rejected().is_empty(),
        "{:?}",
        world.node().rejected()
    );
}

/// A block the peer mined before a price rise is imported by the primary
/// after it: its transactions are judged at the prices it was mined at, so
/// the import never rejects a block the network accepted.
#[tokio::test(start_paused = true)]
async fn imported_block_keeps_its_mining_prices() {
    let mut world = World::new(Network::Partitions);
    let peer = world.peer.clone().expect("peer");
    let a = world.start("SEQ_A", passive()).await;
    let first = world.publish_included(a, "opens the channel").await;
    world.finalize_tx(first).await;
    world.coin_split(&[5]).await;
    let _ = peer.import_from(world.node());

    // The user pays the exact fee at these prices.
    let notes = world.take_user_notes(&[5]);
    let (hash, _, _) = world.deposit(&notes, "mined before the rise").await;
    world.node().gossip_with(&peer);
    let slot = peer.tip_slot().into_inner() + 1;
    let parent = peer.tip();
    let _ = world
        .mine_block_on(&peer, parent, slot, Posted::All, true)
        .await;
    assert!(
        peer.is_included(&hash),
        "the peer did not mine the deposit: mempool {}, rejected {:?}, same tip {}",
        peer.mempool().len(),
        peer.rejected(),
        peer.tip() == world.node().tip()
    );

    let dearer = GasPrices::new(110, 110);
    world.node().set_gas_prices(dearer.clone());
    peer.set_gas_prices(dearer);
    assert_eq!(world.node().import_from(&peer), 1, "one block imported");
    assert!(
        world.node().is_included(&hash),
        "the imported block carries the deposit"
    );
}
