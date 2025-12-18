use async_stream::stream;
use broadcast_service::BlockInfo;
use common_http_client::CommonHttpClient;
use demo_sequencer::{BlockData, db::AccountDb};
use futures::{Stream, StreamExt as _};
use nomos_core::{
    header::HeaderId,
    mantle::{
        Op, SignedMantleTx,
        ops::channel::{ChannelId, inscribe::InscriptionOp},
    },
};
use owo_colors::OwoColorize as _;
use serde::{Deserialize, Serialize};
use tokio::select;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::db::BlockStore;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L2BlockInfo {
    pub data: BlockData,
    pub l1_block_id: HeaderId,
}

pub struct BlockStream;

impl BlockStream {
    pub fn create(
        cancellation_token: CancellationToken,
        http_client: CommonHttpClient,
        endpoint_url: &Url,
        channel_id: &ChannelId,
        token_name: &str,
    ) -> impl Stream<Item = L2BlockInfo> {
        #[expect(tail_expr_drop_order, reason = "Generated internally by stream macro.")]
        let block_stream = stream! {
            let mut lib_stream = Box::pin(http_client
                .get_lib_stream(endpoint_url.clone())
                .await.unwrap());

            loop {
                select! {
                    // Always poll cancellation token first.
                    biased;

                    () = cancellation_token.cancelled() => {
                        break;
                    }

                    block_info = lib_stream.next() => {
                        let Some(BlockInfo { header_id, height }) = block_info else {
                            println!(
                                "  {} Stream ended unexpectedly",
                                "⚠️".yellow()
                            );
                            break;
                        };

                        println!("  {} Block at height {} ({})","🔗".blue(),
                            height.to_string().bright_white().bold(),
                            &hex::encode(header_id.as_ref()
                        ).dimmed());

                        let block = http_client.get_block_by_id(endpoint_url.clone(), header_id).await.unwrap().unwrap();
                        for l2_block in extract_l2_blocks(block.transactions().cloned(), channel_id, token_name) {
                            yield L2BlockInfo {
                                data: l2_block,
                                l1_block_id: block.header().id()
                            };
                        }
                    }
                }
            }
        };

        block_stream
    }
}

fn extract_l2_blocks(
    block_txs: impl Iterator<Item = SignedMantleTx>,
    decoded_channel_id: &ChannelId,
    token_name: &str,
) -> Vec<BlockData> {
    let block_channel_ops: Vec<BlockData> = block_txs
        .flat_map(|tx| tx.mantle_tx.ops)
        .filter_map(|op| match op {
            Op::ChannelInscribe(InscriptionOp {
                channel_id,
                inscription,
                ..
            }) if &channel_id == decoded_channel_id => {
                Some(serde_json::from_slice::<BlockData>(&inscription).unwrap())
            }
            _ => None,
        })
        .collect();

    if block_channel_ops.is_empty() {
        println!("  {} No inscriptions in this block", "○".dimmed());
    } else {
        for block_data in &block_channel_ops {
            println!("{}", "┌".to_owned().bright_green());
            println!(
                "│ {} Block #{}",
                "📦".green(),
                block_data.block_id.to_string().bright_green().bold()
            );
            println!(
                "│ 💳 {} transaction(s)",
                block_data.transactions.len().to_string().yellow().bold()
            );

            for tx_item in &block_data.transactions {
                println!(
                    "│   {} {} → {} ({} {})",
                    "↳".dimmed(),
                    tx_item.from.bright_cyan(),
                    tx_item.to.bright_magenta(),
                    tx_item.amount.to_string().yellow(),
                    token_name
                );
            }
            println!("{}", "└".to_owned().bright_green());
        }
    }

    block_channel_ops
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ValidatedBlockData(BlockData);

impl AsRef<BlockData> for ValidatedBlockData {
    fn as_ref(&self) -> &BlockData {
        &self.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ValidatedL2Info(L2BlockInfo);

impl ValidatedL2Info {
    pub fn new(validated_block_data: ValidatedBlockData, l1_block_id: HeaderId) -> Self {
        Self(L2BlockInfo {
            data: validated_block_data.0,
            l1_block_id,
        })
    }
}

impl AsRef<L2BlockInfo> for ValidatedL2Info {
    fn as_ref(&self) -> &L2BlockInfo {
        &self.0
    }
}

pub async fn validate_block(
    block: BlockData,
    accounts_db: &AccountDb,
    blocks_db: &BlockStore,
) -> Result<ValidatedBlockData, BlockData> {
    // We consider block `0` to be the genesis block and always valid, hence its
    // children won't be checked against the DB.
    if block.parent_block_id > 0
        && !blocks_db
            .is_block_valid(block.parent_block_id)
            .await
            .unwrap()
    {
        return Err(block);
    }

    let are_txs_valid = accounts_db
        .try_apply_transfers(
            block
                .transactions
                .iter()
                .map(|tx| (tx.from.as_str(), tx.to.as_str(), tx.amount)),
        )
        .await
        .is_ok();
    if !are_txs_valid {
        return Err(block);
    }

    Ok(ValidatedBlockData(block))
}
