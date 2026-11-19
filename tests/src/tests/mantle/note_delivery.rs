use std::time::Duration;

use lb_core::mantle::{
    Note, OpRef,
    note_set::NoteSetIndex,
    notes::NoteCommitment,
    traits::Hashable as _,
    transactions::{UNITS_PER_LGO, hash::TxHash},
};
use lb_groth16::{fr_from_bytes, fr_to_bytes};
use lb_http_api_common::bodies::wallet::{
    delivery::{
        WalletAddressResponseBody, WalletImportDeliveryMessagesRequestBody,
        WalletImportDeliveryMessagesResponseBody, WalletNameStakingKeyRequestBody,
        WalletRedeliverRequestBody, WalletRedeliverResponseBody,
        WalletTransferFundsToAddressRequestBody, WalletTransferFundsToAddressResponseBody,
    },
    import::WalletImportNoteRequestBody,
    transfer_funds::WalletTransferFundsRequestBody,
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_testing_framework::{
    NodeHttpClient,
    configs::wallet::{WalletAccount, WalletConfig},
};
use lb_wallet::{
    address::{Address, Network},
    delivery::{DeliveryMessage, delivery_inscription},
};
use logos_blockchain_tests::common::{
    chain::scan_chain_until,
    manual_cluster::{
        api_url, get_wallet_balance, start_fast_cluster_with_wallet, wait_for_nodes_height,
    },
};
use reqwest::{Response, StatusCode};
use serde::Serialize;
use serial_test::serial;
use tokio::time::{Instant, sleep};

const PAYER_NOTE: u64 = 10_000 * UNITS_PER_LGO;
const PAYMENTS: u64 = 5;
const RECIPIENT_FUNDS: u64 = 1;
const AMOUNT: u64 = 1_000 * UNITS_PER_LGO;
const TIMEOUT: Duration = Duration::from_mins(6);
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// The blocks after which the block of a payment has left the states a wallet
/// holds: well past the security parameter of the cluster and the few blocks
/// a wallet keeps below its latest immutable block.
const BLOCKS_PAST_THE_HELD_STATES: u64 = 25;

/// A note paid to an address reaches the wallet of its recipient through its
/// delivery message alone, and the recipient spends it. Two nodes hold the
/// same keys, and the first pays the second five times:
///
/// 1. The delivery message of the first payment is lost. The receiver sees the
///    commitment in a block and finds no note. The sender delivers the note
///    again from its records, and the receiver, handed the message after its
///    block, applies the block again. The transfer holds one inscribe, the
///    delivery inscription of the message the sender first made: the second
///    encryption of the note is not the one the chain commits to.
/// 2. The second payment is delivered before its block, and the receiver
///    restarts with the message pending. The wallet opens the message it wrote
///    to storage when the block is applied.
/// 3. Encryption is the choice of the recipient: the third payment goes to its
///    public key alone, and the receiver imports the note it is given in the
///    clear.
/// 4. The receiver named a staking key to the sender: the fourth note is
///    committed to it, and the wallet owns the note because it named the key.
/// 5. The message of the fifth payment arrives after its block has left the
///    states the wallet holds. The wallet keeps it, and finds the note when it
///    is rebuilt over the chain.
///
/// The receiver then pays back more than three of the notes hold, so its
/// transfer spends four of them: the proof verifies only for the commitments
/// the ledger holds, under the nullifier and the path the wallet derived from
/// each note.
#[tokio::test]
#[serial]
async fn a_delivered_note_reaches_the_wallet_of_its_recipient_and_is_spent() {
    let payer = WalletAccount::deterministic(0, PAYER_NOTE, false).expect("valid");
    let recipient = WalletAccount::deterministic(1, RECIPIENT_FUNDS, false).expect("valid");
    let (payer_pk, recipient_pk) = (payer.public_key(), recipient.public_key());
    // The payer holds one note for each payment: a note funding a transaction
    // is reserved until the transaction is in a block.
    let more_payer_notes = (1..PAYMENTS).map(|index| {
        WalletAccount::new(
            format!("payer-{index}"),
            payer.secret_key.clone(),
            PAYER_NOTE,
            false,
        )
        .expect("valid")
    });
    let wallet_config = WalletConfig::new(
        std::iter::once(payer.clone())
            .chain(more_payer_notes)
            .chain(std::iter::once(recipient))
            .collect(),
    );

    let (base, nodes) =
        start_fast_cluster_with_wallet("note-delivery", "mantle-note-delivery", 2, wallet_config)
            .await;
    let (sender, receiver) = (&nodes[0].client, &nodes[1].client);
    wait_for_nodes_height(&[sender, receiver], 3, Duration::from_mins(5)).await;

    let address = address(receiver, recipient_pk).await;
    assert!(address.starts_with("tlogos1"));
    assert_eq!(address.len(), 2012, "the full address of a test network");
    let balance = |payments: u64| RECIPIENT_FUNDS + payments * AMOUNT;

    // What is not an address, a delivery message or a note of the wallet is
    // refused.
    let wrong_address = address
        .chars()
        .enumerate()
        .map(|(index, character)| match (index, character) {
            (100, 'q') => 'p',
            (100, _) => 'q',
            _ => character,
        })
        .collect::<String>();
    assert_eq!(
        post(
            sender,
            TRANSFER_TO_ADDRESS,
            &transfer_body(payer_pk, &wrong_address, None)
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post(
            receiver,
            IMPORT_DELIVERY_MESSAGES,
            &WalletImportDeliveryMessagesRequestBody {
                messages: vec!["00".repeat(1207)],
            },
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post(
            sender,
            REDELIVER_NOTE,
            &WalletRedeliverRequestBody {
                commitment: NoteCommitment(lb_groth16::Fr::from(1u64)),
                recipient_address: address.clone(),
            },
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );

    // 1. The message of the first payment is lost, and its block is applied
    // by both nodes: the chain alone gives the wallet of the receiver nothing.
    let first = transfer_to(sender, payer_pk, &address, None).await;
    let lost = first
        .delivery_message
        .expect("a full address is encrypted to");
    assert_eq!(lost.len(), 2 * 1208, "a delivery message is 1208 bytes");
    wait_for_balance(sender, recipient_pk, |held| held == balance(1)).await;
    assert_eq!(
        inscriptions_of(sender, first.hash).await,
        [inscription_of(&lost)]
    );
    wait_for_nodes_height(&[receiver], height(sender).await, TIMEOUT).await;
    assert_eq!(get_wallet_balance(receiver, recipient_pk).await, balance(0));
    // The sender kept the note, and encrypts it again: another ciphertext
    // for the same commitment.
    let redelivered: WalletRedeliverResponseBody = post(
        sender,
        REDELIVER_NOTE,
        &WalletRedeliverRequestBody {
            commitment: commitment_of(&lost),
            recipient_address: address.clone(),
        },
    )
    .await
    .json()
    .await
    .expect("the redelivery response should be valid JSON");
    let redelivered = redelivered
        .delivery_message
        .expect("a full address is encrypted to");
    assert_ne!(redelivered, lost);
    assert_eq!(commitment_of(&redelivered), commitment_of(&lost));
    assert_ne!(inscription_of(&redelivered), inscription_of(&lost));
    // The message arrives after its block: the wallet applies the block again
    // and finds the note.
    assert_eq!(import(receiver, &redelivered).await, 1);
    wait_for_balance(receiver, recipient_pk, |held| held == balance(1)).await;

    // 2. The second payment is delivered before its block, and the receiver
    // restarts with the message pending: it was written to storage, so the
    // wallet still opens it when its block is applied.
    let delivered = transfer_to(sender, payer_pk, &address, None)
        .await
        .delivery_message
        .expect("a full address is encrypted to");
    assert_eq!(import(receiver, &delivered).await, 1);
    let cluster = base.cluster();
    let receiver_name = &nodes[1].name;
    cluster
        .restart_node(receiver_name)
        .await
        .expect("the receiver should restart");
    cluster
        .wait_node_ready(receiver_name)
        .await
        .expect("the receiver should become ready after its restart");
    let receiver = &cluster
        .node_client(receiver_name)
        .expect("the client of the receiver should be available after its restart");
    wait_for_balance(receiver, recipient_pk, |held| held == balance(2)).await;

    // 3. The public key alone: no ciphertext, the note is handed over as it
    // is.
    let key_only = Address {
        public_key: recipient_pk,
        encryption_key: None,
    }
    .encode(Network::Other);
    let in_the_clear = transfer_to(sender, payer_pk, &key_only, None).await;
    assert_eq!(in_the_clear.delivery_message, None);
    let note = in_the_clear
        .recipient_note
        .expect("a key-only address gets its note in the clear");
    import_note(receiver, note).await;
    wait_for_balance(receiver, recipient_pk, |held| held == balance(3)).await;
    assert_eq!(
        inscriptions_of(sender, in_the_clear.hash).await,
        [] as [Vec<u8>; 0],
        "a note handed over in the clear has no delivery message to commit to"
    );

    // 4. The receiver names a staking key, here the key of the payer, and
    // the sender commits the note to it. The ciphertext carries no key: the
    // wallet owns the note because it tries the keys it named.
    let named = post(
        receiver,
        NAME_STAKING_KEY,
        &WalletNameStakingKeyRequestBody {
            staking_key: payer_pk,
        },
    )
    .await;
    assert!(named.status().is_success(), "{}", named.status());
    let staked = transfer_to(sender, payer_pk, &address, Some(payer_pk))
        .await
        .delivery_message
        .expect("a full address is encrypted to");
    assert_eq!(import(receiver, &staked).await, 1);
    wait_for_balance(receiver, recipient_pk, |held| held == balance(4)).await;

    // 5. The message of the fifth payment arrives once its block is older
    // than the states the wallet holds: applying held blocks again does not
    // find the note, and the wallet keeps the message.
    let old = transfer_to(sender, payer_pk, &address, None)
        .await
        .delivery_message
        .expect("a full address is encrypted to");
    wait_for_balance(sender, recipient_pk, |held| held == balance(5)).await;
    let included = height(sender).await;
    wait_for_nodes_height(&[receiver], included + BLOCKS_PAST_THE_HELD_STATES, TIMEOUT).await;
    assert_eq!(import(receiver, &old).await, 1);
    wait_for_nodes_height(&[receiver], height(receiver).await + 2, TIMEOUT).await;
    assert_eq!(get_wallet_balance(receiver, recipient_pk).await, balance(4));
    // Rebuilt over the chain, the wallet opens the message at its block.
    let rescanned = post(receiver, RESCAN, &()).await;
    assert!(rescanned.status().is_success(), "{}", rescanned.status());
    wait_for_balance(receiver, recipient_pk, |held| held == balance(5)).await;

    // A transfer spends at most four notes. More than three of them hold
    // takes four of the five, each learnt in its own way, and leaves most of
    // the fourth for the fee: the storage fee of the transfer and the margin
    // the wallet funds above it come close to half a note. A note is
    // spendable once it is a few blocks below the tip.
    let spent = 3 * AMOUNT + AMOUNT / 100;
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let transfer = receiver
            .transfer_funds(WalletTransferFundsRequestBody {
                tip: None,
                change_public_key: recipient_pk,
                funding_public_keys: vec![recipient_pk],
                recipient_public_key: payer_pk,
                amount: spent,
            })
            .await;
        let Err(error) = transfer else {
            break;
        };
        assert!(
            Instant::now() < deadline,
            "the recipient should fund a transfer with the delivered notes: {error:?}"
        );
        sleep(POLL_INTERVAL).await;
    }
    // The spend is in a block once the wallet holds its change in place of
    // the notes: what is left, less the fee.
    wait_for_balance(receiver, recipient_pk, |held| {
        held > 0 && held <= balance(5) - spent
    })
    .await;
}

const TRANSFER_TO_ADDRESS: &str = "wallet/transactions/transfer-funds-to-address";
const IMPORT_DELIVERY_MESSAGES: &str = "wallet/delivery-messages/import";
const REDELIVER_NOTE: &str = "wallet/notes/redeliver";
const NAME_STAKING_KEY: &str = "wallet/staking-keys/name";
const RESCAN: &str = "wallet/rescan";

async fn post(node: &NodeHttpClient, path: &str, body: &(impl Serialize + Sync)) -> Response {
    reqwest::Client::new()
        .post(api_url(node, path))
        .json(body)
        .send()
        .await
        .expect("the request should not fail")
}

async fn height(node: &NodeHttpClient) -> u64 {
    node.consensus_info()
        .await
        .expect("the node should give its height")
        .cryptarchia_info
        .height
}

// The inscription a transfer commits to a delivery message, in hex, with.
fn inscription_of(delivery_message: &str) -> Vec<u8> {
    let message = DeliveryMessage::from_bytes(&hex::decode(delivery_message).expect("hex"))
        .expect("a delivery message is 1208 bytes");
    delivery_inscription(&[message]).to_vec()
}

// The inscriptions of the inscribes of the transaction `hash`, once a block of
// the chain of `node` holds it. The chain is read again until it does: a tip
// can move to a branch that does not hold the transaction yet, and a block
// just applied may not be served yet.
async fn inscriptions_of(node: &NodeHttpClient, hash: TxHash) -> Vec<Vec<u8>> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let tip = node
            .consensus_info()
            .await
            .expect("the node should give its tip")
            .cryptarchia_info
            .tip;
        let found = scan_chain_until(
            tip,
            &mut std::collections::HashSet::new(),
            async |header_id| node.block(&header_id).await.ok().flatten(),
            |block| {
                let transaction = block
                    .transactions
                    .iter()
                    .find(|transaction| transaction.hash() == hash)?;
                Some(
                    transaction
                        .op_refs_iter()
                        .filter_map(|op| match op {
                            OpRef::Inscribe(inscribe) => Some(inscribe.inscription.to_vec()),
                            _ => None,
                        })
                        .collect(),
                )
            },
        )
        .await;
        if let Some(inscriptions) = found {
            return inscriptions;
        }
        assert!(
            Instant::now() < deadline,
            "no block of the chain holds the transaction"
        );
        sleep(POLL_INTERVAL).await;
    }
}

// The commitment a delivery message, in hex, starts with.
fn commitment_of(delivery_message: &str) -> NoteCommitment {
    let bytes = hex::decode(delivery_message).expect("hex");
    NoteCommitment(fr_from_bytes(&bytes[..32]).expect("a commitment is a field element"))
}

async fn address(node: &NodeHttpClient, public_key: ZkPublicKey) -> String {
    let public_key = hex::encode(fr_to_bytes(&public_key.into()));
    let response = reqwest::Client::new()
        .get(api_url(node, &format!("wallet/{public_key}/address")))
        .send()
        .await
        .expect("the address request should not fail");
    assert!(response.status().is_success(), "{}", response.status());
    response
        .json::<WalletAddressResponseBody>()
        .await
        .expect("the address response should be valid JSON")
        .address
}

fn transfer_body(
    payer: ZkPublicKey,
    address: &str,
    staking_key: Option<ZkPublicKey>,
) -> WalletTransferFundsToAddressRequestBody {
    WalletTransferFundsToAddressRequestBody {
        tip: None,
        change_public_key: payer,
        funding_public_keys: vec![payer],
        recipient_address: address.to_owned(),
        recipient_stake_public_key: staking_key,
        amount: AMOUNT,
    }
}

// Pays `AMOUNT` from `payer` to `address`, the note taking `staking_key` as
// staking key when there is one.
async fn transfer_to(
    node: &NodeHttpClient,
    payer: ZkPublicKey,
    address: &str,
    staking_key: Option<ZkPublicKey>,
) -> WalletTransferFundsToAddressResponseBody {
    let response = post(
        node,
        TRANSFER_TO_ADDRESS,
        &transfer_body(payer, address, staking_key),
    )
    .await;
    assert!(response.status().is_success(), "{}", response.status());
    response
        .json()
        .await
        .expect("the transfer response should be valid JSON")
}

// Hands the wallet of `node` a note it is paid, in the clear.
async fn import_note(node: &NodeHttpClient, note: Note) {
    let response = post(
        node,
        "wallet/notes/import",
        &WalletImportNoteRequestBody {
            note,
            note_set: NoteSetIndex::Ledger,
        },
    )
    .await;
    assert!(response.status().is_success(), "{}", response.status());
}

// Hands `message` to the wallet of `node`, and tells how many messages it
// keeps.
async fn import(node: &NodeHttpClient, message: &str) -> usize {
    let response = post(
        node,
        IMPORT_DELIVERY_MESSAGES,
        &WalletImportDeliveryMessagesRequestBody {
            messages: vec![message.to_owned()],
        },
    )
    .await;
    assert!(response.status().is_success(), "{}", response.status());
    response
        .json::<WalletImportDeliveryMessagesResponseBody>()
        .await
        .expect("the import response should be valid JSON")
        .kept
}

async fn wait_for_balance(
    node: &NodeHttpClient,
    public_key: ZkPublicKey,
    reached: impl Fn(u64) -> bool,
) {
    let deadline = Instant::now() + TIMEOUT;
    let mut balance = get_wallet_balance(node, public_key).await;
    while !reached(balance) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the balance, last balance was {balance}"
        );
        sleep(POLL_INTERVAL).await;
        balance = get_wallet_balance(node, public_key).await;
    }
}
