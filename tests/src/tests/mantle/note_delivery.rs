use std::time::Duration;

use lb_core::mantle::{note_set::NoteSetIndex, transactions::UNITS_PER_LGO};
use lb_groth16::fr_to_bytes;
use lb_http_api_common::bodies::wallet::{
    delivery::{
        WalletAddressResponseBody, WalletImportDeliveryMessagesRequestBody,
        WalletImportDeliveryMessagesResponseBody, WalletTransferFundsToAddressRequestBody,
        WalletTransferFundsToAddressResponseBody,
    },
    import::WalletImportNoteRequestBody,
    transfer_funds::WalletTransferFundsRequestBody,
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_testing_framework::{
    NodeHttpClient,
    configs::wallet::{WalletAccount, WalletConfig},
};
use lb_wallet::address::{Address, Network};
use logos_blockchain_tests::common::manual_cluster::{
    api_url, get_wallet_balance, start_fast_cluster_with_wallet, wait_for_nodes_height,
};
use serial_test::serial;
use tokio::time::{Instant, sleep};

const PAYER_NOTE: u64 = 10_000 * UNITS_PER_LGO;
const RECIPIENT_FUNDS: u64 = 1;
const AMOUNT: u64 = 1_000 * UNITS_PER_LGO;
const TIMEOUT: Duration = Duration::from_mins(6);
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// A note paid to an address reaches the wallet of its recipient through its
/// delivery message alone, and the recipient spends it:
///
/// 1. Two nodes hold the same keys. The sender pays the recipient twice, from
///    the first node, to the address the second node gives.
/// 2. The second node is handed the delivery message of the second payment
///    only. Its wallet opens it once a block holds its commitment, and counts
///    that note and not the other, whose commitment it sees and cannot open.
/// 3. Encryption is the choice of the recipient: a third payment goes to its
///    public key alone, and the second node imports the note it is given in the
///    clear.
/// 4. The second node pays back more than either note holds, so its transfer
///    spends both: the proof verifies only for the commitments the ledger
///    holds, under the nullifier and the path the wallet derived from the
///    decrypted note.
#[tokio::test]
#[serial]
async fn a_delivered_note_reaches_the_wallet_of_its_recipient_and_is_spent() {
    let payer = WalletAccount::deterministic(0, PAYER_NOTE, false).expect("valid");
    let recipient = WalletAccount::deterministic(1, RECIPIENT_FUNDS, false).expect("valid");
    let (payer_pk, recipient_pk) = (payer.public_key(), recipient.public_key());
    // The payer holds three notes, one for each payment: a note funding a
    // transaction is reserved until the transaction is in a block.
    let more_payer_notes = (1..3).map(|index| {
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

    let (_base, nodes) =
        start_fast_cluster_with_wallet("note-delivery", "mantle-note-delivery", 2, wallet_config)
            .await;
    let (sender, receiver) = (&nodes[0].client, &nodes[1].client);
    wait_for_nodes_height(&[sender, receiver], 3, Duration::from_mins(5)).await;

    let address = address(receiver, recipient_pk).await;
    assert!(address.starts_with("tlogos1"));
    assert_eq!(address.len(), 2012, "the full address of a test network");

    let withheld = transfer_to(sender, payer_pk, &address).await;
    let delivered = transfer_to(sender, payer_pk, &address).await;
    let (withheld, delivered) = (
        withheld
            .delivery_message
            .expect("a full address is encrypted to"),
        delivered
            .delivery_message
            .expect("a full address is encrypted to"),
    );
    assert_ne!(withheld, delivered);
    assert_eq!(
        delivered.len(),
        2 * 1208,
        "a delivery message is 1208 bytes"
    );
    assert_eq!(import(receiver, &delivered).await, 1);

    // The public key alone: no ciphertext, the note is handed over as it is.
    let key_only = Address {
        public_key: recipient_pk,
        encryption_key: None,
    }
    .encode(Network::Other);
    let in_the_clear = transfer_to(sender, payer_pk, &key_only).await;
    assert_eq!(in_the_clear.delivery_message, None);
    let note = in_the_clear
        .recipient_note
        .expect("a key-only address gets its note in the clear");
    import_note(receiver, note).await;

    // The first node built the three payments and knows their notes. The
    // second node learns two of them: one from its delivery message, one in
    // the clear.
    wait_for_balance(sender, recipient_pk, |balance| {
        balance == RECIPIENT_FUNDS + 3 * AMOUNT
    })
    .await;
    wait_for_balance(receiver, recipient_pk, |balance| {
        balance == RECIPIENT_FUNDS + 2 * AMOUNT
    })
    .await;

    // The two notes fund a payment of more than either holds, so both are
    // spent. A note is spendable once it is a few blocks below the tip.
    let spent = AMOUNT + AMOUNT / 2;
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
            "the recipient should fund a transfer with the delivered note: {error:?}"
        );
        sleep(POLL_INTERVAL).await;
    }
    // The spend is in a block once the wallet holds its change in place of
    // the delivered note: what is left, less the fee.
    wait_for_balance(receiver, recipient_pk, |balance| {
        balance > 0 && balance <= RECIPIENT_FUNDS + 2 * AMOUNT - spent
    })
    .await;
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

// Pays `AMOUNT` from `payer` to `address`.
async fn transfer_to(
    node: &NodeHttpClient,
    payer: ZkPublicKey,
    address: &str,
) -> WalletTransferFundsToAddressResponseBody {
    let response = reqwest::Client::new()
        .post(api_url(
            node,
            "wallet/transactions/transfer-funds-to-address",
        ))
        .json(&WalletTransferFundsToAddressRequestBody {
            tip: None,
            change_public_key: payer,
            funding_public_keys: vec![payer],
            recipient_address: address.to_owned(),
            amount: AMOUNT,
        })
        .send()
        .await
        .expect("the transfer request should not fail");
    assert!(response.status().is_success(), "{}", response.status());
    response
        .json()
        .await
        .expect("the transfer response should be valid JSON")
}

// Hands the wallet of `node` a note it is paid, in the clear.
async fn import_note(node: &NodeHttpClient, note: lb_core::mantle::Note) {
    let response = reqwest::Client::new()
        .post(api_url(node, "wallet/notes/import"))
        .json(&WalletImportNoteRequestBody {
            note,
            note_set: NoteSetIndex::Ledger,
        })
        .send()
        .await
        .expect("the import request should not fail");
    assert!(response.status().is_success(), "{}", response.status());
}

// Hands `message` to the wallet of `node`, and tells how many messages it
// keeps.
async fn import(node: &NodeHttpClient, message: &str) -> usize {
    let response = reqwest::Client::new()
        .post(api_url(node, "wallet/delivery-messages/import"))
        .json(&WalletImportDeliveryMessagesRequestBody {
            messages: vec![message.to_owned()],
        })
        .send()
        .await
        .expect("the import request should not fail");
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
