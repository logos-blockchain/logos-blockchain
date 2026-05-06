use lb_key_management_system_keys::keys::{ZkPublicKey, ZkSignature};
use serde::{Deserialize, Serialize};

use crate::{
    mantle::{
        TxHash,
        channel::{Channels, Error},
        ledger::{Inputs, Operation, Utxos},
        ops::channel::ChannelId,
    },
    sdp::locked_notes::LockedNotes,
};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct DepositOp {
    pub channel_id: ChannelId,
    pub inputs: Inputs,
    pub metadata: Vec<u8>,
}

pub struct DepositValidationContext<'a> {
    pub channels: &'a Channels,
    pub locked_notes: &'a LockedNotes,
    pub utxos: &'a Utxos,
    pub tx_hash: &'a TxHash,
    pub deposit_sig: &'a ZkSignature,
}

pub struct DepositExecutionContext {
    pub channels: Channels,
    pub locked_notes: LockedNotes,
    pub utxos: Utxos,
}

impl Operation for DepositOp {
    type ValidationContext<'a>
        = DepositValidationContext<'a>
    where
        Self: 'a;
    type ExecutionContext<'a>
        = DepositExecutionContext
    where
        Self: 'a;
    type Error = Error;

    fn validate(&self, ctx: &Self::ValidationContext<'_>) -> Result<(), Self::Error> {
        // Check that the channel exist
        if !ctx.channels.channels.contains_key(&self.channel_id) {
            return Err(Error::ChannelNotFound {
                channel_id: self.channel_id,
            });
        }

        // Check that inputs are valid
        self.inputs.validate(ctx.locked_notes, ctx.utxos)?;

        // Check the signature
        let pks = self.inputs.get_pk(ctx.utxos)?;
        if !ZkPublicKey::verify_multi(&pks, &ctx.tx_hash.to_fr(), ctx.deposit_sig) {
            return Err(Error::InvalidSignature);
        }

        Ok(())
    }

    fn execute(
        &self,
        mut ctx: Self::ExecutionContext<'_>,
    ) -> Result<Self::ExecutionContext<'_>, Self::Error> {
        // Get the amount deposited
        let amount_deposited = self.inputs.amount(&ctx.utxos)?;

        // Remove inputs from the ledger
        ctx.utxos = self.inputs.execute(ctx.utxos)?;

        // Increase the balance of the channel
        if let Some(channel) = ctx.channels.channels.get_mut(&self.channel_id) {
            channel.balance = channel
                .balance
                .checked_add(amount_deposited)
                .ok_or(Error::BalanceOverflow)?;
            Ok(self)
        } else {
            Err(Error::ChannelNotFound {
                channel_id: self.channel_id,
            })
        }?;

        Ok(ctx)
    }
}
