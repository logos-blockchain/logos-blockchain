use lb_key_management_system_keys::keys::Ed25519Signature;
use serde::{Deserialize, Serialize};

use super::{ChannelId, Ed25519PublicKey, MsgId};
use crate::mantle::{
    TxHash,
    channel::{ChannelState, Channels, DEFAULT_WITHDRAW_THRESHOLD, Error},
    ledger::Operation,
};
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct SetKeysOp {
    pub channel: ChannelId,
    pub keys: Vec<Ed25519PublicKey>,
}

pub struct SetKeysValidationContext<'a> {
    pub channels: &'a Channels,
    pub tx_hash: &'a TxHash,
    pub setkeys_sig: &'a Ed25519Signature,
}

// TODO: Replace with CHANNEL_CONFIG op: https://github.com/logos-blockchain/logos-blockchain/issues/2461
impl Operation<SetKeysValidationContext<'_>> for SetKeysOp {
    type ExecutionContext<'a>
        = Channels
    where
        Self: 'a;
    type Error = Error;

    fn validate(&self, ctx: &SetKeysValidationContext<'_>) -> Result<(), Self::Error> {
        // Check that the list of key isn't empty
        if self.keys.is_empty() {
            return Err(Error::EmptyKeys {
                channel_id: self.channel,
            });
        }

        // Check that the signature is valid against the first public key of the list
        if let Some(channel) = ctx.channels.channels.get(&self.channel)
            && channel.keys[0]
                .verify(ctx.tx_hash.as_signing_bytes().as_ref(), ctx.setkeys_sig)
                .is_err()
        {
            return Err(Error::InvalidSignature);
        }

        Ok(())
    }

    fn execute(
        &self,
        mut channels: Self::ExecutionContext<'_>,
    ) -> Result<Self::ExecutionContext<'_>, Self::Error> {
        // if the channel doesn't exist, create it other just change the keys
        if let Some(channel) = channels.channels.get_mut(&self.channel) {
            channel.keys = self.keys.clone().into();
        } else {
            channels.channels = channels.channels.insert(
                self.channel,
                ChannelState {
                    tip: MsgId::root(),
                    keys: self.keys.clone().into(),
                    balance: 0,
                    // TODO: Replace with `ChannelConfig.withdraw_threshold`
                    // once this op is replaced with CHANNEL_CONFIG op: https://github.com/logos-blockchain/logos-blockchain/issues/2461
                    withdraw_threshold: DEFAULT_WITHDRAW_THRESHOLD,
                    withdrawal_nonce: 0,
                },
            );
        }
        Ok(channels)
    }
}
