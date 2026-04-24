use serde::{Deserialize, Serialize};

use crate::{
    mantle::{
        TxHash,
        channel::{Channels, Error},
        encoding::encode_channel_withdraw,
        ledger::{Operation, Outputs, Utxos},
        ops::{OpId, channel::ChannelId},
    },
    proofs::channel_withdraw_proof::ChannelWithdrawProof,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChannelWithdrawOp {
    pub channel_id: ChannelId,
    pub outputs: Outputs,
    pub withdraw_nonce: u32,
}

impl OpId for ChannelWithdrawOp {
    fn op_bytes(&self) -> Vec<u8> {
        encode_channel_withdraw(self)
    }
}

pub struct WithdrawValidationContext<'a> {
    pub channels: &'a Channels,
    pub tx_hash: &'a TxHash,
    pub withdraw_sigs: &'a ChannelWithdrawProof,
}

pub struct WithdrawExecutionContext {
    pub channels: Channels,
    pub utxos: Utxos,
}

impl Operation for ChannelWithdrawOp {
    type ValidationContext<'a>
        = WithdrawValidationContext<'a>
    where
        Self: 'a;
    type ExecutionContext<'a>
        = WithdrawExecutionContext
    where
        Self: 'a;
    type Error = Error;

    fn validate(&self, ctx: &Self::ValidationContext<'_>) -> Result<(), Self::Error> {
        // Check that the outputs are valid
        self.outputs.validate()?;

        // Check that the channel exist
        if !ctx.channels.channels.contains_key(&self.channel_id) {
            return Err(Error::ChannelNotFound {
                channel_id: self.channel_id,
            });
        }

        // Check that the withdrawal nonce is correct
        let channel = ctx
            .channels
            .channels
            .get(&self.channel_id)
            .cloned()
            .expect("we checked that the channel exist above");
        if channel.withdrawal_nonce != self.withdraw_nonce {
            return Err(Error::InvalidWithdrawNonce);
        }

        // Check that the channel has enough funds
        let amount = self.outputs.amount()?;
        if amount > channel.balance {
            return Err(Error::InsufficientFunds);
        }

        // Check that there is enough signatures and that the indexes are unique
        // This is enforced by the structure that enforces it

        // Check the signature
        let signatures = ctx.withdraw_sigs.signatures();
        if signatures.len() != channel.withdraw_threshold as usize {
            return Err(Error::WithdrawThresholdUnmet {
                channel_id: self.channel_id,
                threshold: channel.withdraw_threshold,
                actual: ctx.withdraw_sigs.signatures().len(),
            });
        }

        // Check the signatures
        for sig in signatures {
            if channel.keys[sig.channel_key_index as usize]
                .verify(ctx.tx_hash.as_signing_bytes().as_ref(), &sig.signature)
                .is_err()
            {
                return Err(Error::InvalidSignature);
            }
        }

        Ok(())
    }

    fn execute(
        &self,
        mut ctx: Self::ExecutionContext<'_>,
    ) -> Result<Self::ExecutionContext<'_>, Self::Error> {
        // Get the amount withdraw
        let amount_withdraw = self.outputs.amount()?;

        // Decrease the balance of the channel and increase the withdrawal nonce
        if let Some(channel) = ctx.channels.channels.get_mut(&self.channel_id) {
            channel.balance = channel
                .balance
                .checked_sub(amount_withdraw)
                .ok_or(Error::InsufficientFunds)?;
            channel.withdrawal_nonce = channel
                .withdrawal_nonce
                .checked_add(1)
                .ok_or(Error::WithdrawNonceOverflow)?;
            Ok(self)
        } else {
            Err(Error::ChannelNotFound {
                channel_id: self.channel_id,
            })
        }?;

        // Add the ouputs to the ledger
        ctx.utxos = self.outputs.execute(ctx.utxos, self);

        Ok(ctx)
    }
}
