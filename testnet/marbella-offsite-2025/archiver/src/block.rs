use async_stream::stream;
use broadcast_service::BlockInfo;
use common_http_client::CommonHttpClient;
use demo_sequencer::BlockData;
use futures::{Stream, StreamExt as _};
use nomos_core::{
    block::Block,
    mantle::{
        Op, SignedMantleTx,
        ops::channel::{ChannelId, inscribe::InscriptionOp},
    },
};
use owo_colors::OwoColorize as _;
use tokio::select;
use tokio_util::sync::CancellationToken;
use url::Url;

pub struct BlockStream;

impl BlockStream {
    pub fn create(
        cancellation_token: CancellationToken,
        http_client: CommonHttpClient,
        endpoint_url: &Url,
        channel_id: &ChannelId,
        token_name: &str,
    ) -> impl Stream<Item = BlockData> {
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
                        for l2_block in extract_l2_blocks(block, channel_id, token_name) {
                            yield l2_block;
                        }
                    }
                }
            }
        };

        block_stream
    }
}

fn extract_l2_blocks(
    block: Block<SignedMantleTx>,
    decoded_channel_id: &ChannelId,
    token_name: &str,
) -> Vec<BlockData> {
    let block_channel_ops: Vec<BlockData> = block
        .into_transactions()
        .into_iter()
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
