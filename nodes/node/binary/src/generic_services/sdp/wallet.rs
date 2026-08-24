use lb_core::{
    mantle::{
        Op, SignedMantleTx,
        gas::FeeHorizonHours,
        transactions::{MantleTxBuilder, states::Preverified},
    },
    sdp::{ActiveMessage, DeclarationMessage, WithdrawMessage},
};
use lb_sdp_service::wallet::{
    SdpWalletAdapter as SdpWalletAdapterTrait, SdpWalletConfig, SdpWalletError,
};
use lb_wallet_service::{
    TipResponse,
    api::{WalletApi, WalletServiceData},
};
use overwatch::services::{AsServiceId, ServiceData, relay::OutboundRelay};

// The horizon covers deterministic storage-fee movement; the independent
// priority reserve remains an explicit incentive. Any retry/rebuild would
// handle only exceptional lifetime, execution-fee, submission, or
// state-invalidating cases.
const SDP_PRIORITY_FEE_PERCENT: u64 = 12;
const SDP_FEE_HORIZON_HOURS: FeeHorizonHours = FeeHorizonHours::from_tenths(15);

pub struct SdpWalletAdapter<Service, RuntimeServiceId>
where
    Service: WalletServiceData,
{
    api: WalletApi<Service, RuntimeServiceId>,
}

#[async_trait::async_trait]
impl<S, R> SdpWalletAdapterTrait for SdpWalletAdapter<S, R>
where
    S: WalletServiceData + Send + Sync,
    S::Message: Send,
    R: AsServiceId<S> + std::fmt::Debug + std::fmt::Display + Sync,
{
    type WalletService = S;

    fn new(outbound_relay: OutboundRelay<<Self::WalletService as ServiceData>::Message>) -> Self {
        Self {
            api: WalletApi::new(outbound_relay),
        }
    }

    async fn declare_tx(
        &self,
        mut tx_builder: MantleTxBuilder,
        declaration: DeclarationMessage,
        config: &SdpWalletConfig,
    ) -> Result<SignedMantleTx<Preverified>, SdpWalletError> {
        tx_builder = tx_builder.push_op(Op::SDPDeclare(declaration))?;

        let TipResponse {
            tip,
            response: funded,
        } = self
            .api
            .fund_tx_with_policy(
                None,
                tx_builder,
                config.funding_pk,
                vec![config.funding_pk],
                SDP_PRIORITY_FEE_PERCENT,
                Some(SDP_FEE_HORIZON_HOURS),
                Some(config.max_tx_fee),
            )
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?;

        let tx_fee = funded.tx_fee()?;
        if tx_fee > config.max_tx_fee {
            return Err(SdpWalletError::TxFeeExceedsMaxFee {
                tx_fee,
                max_fee: config.max_tx_fee,
            });
        }

        let signed_tx = self
            .api
            .sign_tx(Some(tip), funded)
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?
            .response;

        Ok(signed_tx)
    }

    async fn withdraw_tx(
        &self,
        mut tx_builder: MantleTxBuilder,
        withdraw: WithdrawMessage,
        config: &SdpWalletConfig,
    ) -> Result<SignedMantleTx<Preverified>, SdpWalletError> {
        tx_builder = tx_builder.push_op(Op::SDPWithdraw(withdraw))?;

        let TipResponse {
            tip,
            response: funded,
        } = self
            .api
            .fund_tx_with_policy(
                None,
                tx_builder,
                config.funding_pk,
                vec![config.funding_pk],
                SDP_PRIORITY_FEE_PERCENT,
                Some(SDP_FEE_HORIZON_HOURS),
                Some(config.max_tx_fee),
            )
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?;

        let tx_fee = funded.tx_fee()?;
        if tx_fee > config.max_tx_fee {
            return Err(SdpWalletError::TxFeeExceedsMaxFee {
                tx_fee,
                max_fee: config.max_tx_fee,
            });
        }

        let signed_tx = self
            .api
            .sign_tx(Some(tip), funded)
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?
            .response;

        Ok(signed_tx)
    }

    async fn active_tx(
        &self,
        mut tx_builder: MantleTxBuilder,
        active: ActiveMessage,
        config: &SdpWalletConfig,
    ) -> Result<SignedMantleTx<Preverified>, SdpWalletError> {
        tx_builder = tx_builder.push_op(Op::SDPActive(active))?;

        let TipResponse {
            tip,
            response: funded,
        } = self
            .api
            .fund_tx_with_policy(
                None,
                tx_builder,
                config.funding_pk,
                vec![config.funding_pk],
                SDP_PRIORITY_FEE_PERCENT,
                Some(SDP_FEE_HORIZON_HOURS),
                Some(config.max_tx_fee),
            )
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?;

        let tx_fee = funded.tx_fee()?;
        if tx_fee > config.max_tx_fee {
            return Err(SdpWalletError::TxFeeExceedsMaxFee {
                tx_fee,
                max_fee: config.max_tx_fee,
            });
        }

        let signed_tx = self
            .api
            .sign_tx(Some(tip), funded)
            .await
            .map_err(|e| SdpWalletError::WalletApi(e.into()))?
            .response;

        Ok(signed_tx)
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::{self, Display, Formatter};

    use lb_blend::proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection};
    use lb_core::{
        mantle::gas::GasCost,
        sdp::{
            ActivityMetadata, DeclarationId, Locator, ProviderId, ServiceType, blend::ActivityProof,
        },
    };
    use lb_cryptarchia_engine::Epoch;
    use lb_groth16::Fr;
    use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey};
    use lb_wallet_service::{WalletMsg, WalletServiceError, WalletServiceSettings};
    use overwatch::services::state::{NoOperator, NoState};
    use tokio::sync::mpsc;

    use super::*;

    struct DummyWallet;

    impl ServiceData for DummyWallet {
        type Settings = WalletServiceSettings;
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = WalletMsg;
    }

    impl WalletServiceData for DummyWallet {
        type Kms = ();
        type Cryptarchia = ();
        type Tx = ();
        type Storage = ();
    }

    #[derive(Debug)]
    struct TestRuntimeServiceId;

    impl AsServiceId<DummyWallet> for TestRuntimeServiceId {
        const SERVICE_ID: Self = Self;
    }

    impl Display for TestRuntimeServiceId {
        fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
            write!(formatter, "TestRuntimeServiceId")
        }
    }

    fn test_config() -> SdpWalletConfig {
        SdpWalletConfig {
            max_tx_fee: GasCost::new(1234),
            funding_pk: ZkPublicKey::zero(),
        }
    }

    fn declaration() -> DeclarationMessage {
        DeclarationMessage {
            service_type: ServiceType::BlendNetwork,
            locators: [Locator::new_unchecked(
                "/ip4/127.0.0.1/udp/3000/quic-v1".parse().unwrap(),
            )]
            .into(),
            provider_id: ProviderId(Ed25519Key::from_bytes(&[0u8; _]).public_key()),
            zk_id: ZkPublicKey::zero(),
            locked_note_id: Fr::from(0u64).into(),
        }
    }

    fn active() -> ActiveMessage {
        ActiveMessage {
            declaration_id: DeclarationId([0; 32]),
            nonce: 0,
            metadata: ActivityMetadata::Blend(Box::new(ActivityProof {
                epoch: Epoch::new(0),
                signing_key: Ed25519Key::from_bytes(&[0u8; _]).public_key(),
                proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([0; _]).into(),
                proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked([0; _]).into(),
            })),
        }
    }

    fn withdraw() -> WithdrawMessage {
        WithdrawMessage {
            declaration_id: DeclarationId([0; 32]),
            nonce: 0,
            locked_note_id: Fr::from(0u64).into(),
        }
    }

    async fn assert_funding_policy(
        receiver: &mut mpsc::Receiver<WalletMsg>,
        expected_max_tx_fee: GasCost,
    ) {
        let WalletMsg::FundTx {
            priority_fee_percent,
            fee_horizon_hours,
            max_tx_fee,
            resp_tx,
            ..
        } = receiver.recv().await.expect("SDP funding request")
        else {
            panic!("expected SDP funding request");
        };

        assert_eq!(priority_fee_percent, SDP_PRIORITY_FEE_PERCENT);
        assert_eq!(fee_horizon_hours, Some(SDP_FEE_HORIZON_HOURS));
        assert_eq!(max_tx_fee, Some(expected_max_tx_fee));

        resp_tx
            .send(Err(WalletServiceError::KmsApi(
                std::io::Error::other("test").into(),
            )))
            .expect("SDP adapter should still await the funding response");
    }

    async fn assert_operation<Operation, OperationFuture>(
        operation: Operation,
        config: &SdpWalletConfig,
    ) where
        Operation: FnOnce(
                SdpWalletAdapter<DummyWallet, TestRuntimeServiceId>,
                SdpWalletConfig,
            ) -> OperationFuture
            + Send
            + 'static,
        OperationFuture: Future + Send + 'static,
        OperationFuture::Output: Send + 'static,
    {
        let (sender, mut receiver) = mpsc::channel(1);
        let adapter =
            <SdpWalletAdapter<DummyWallet, TestRuntimeServiceId> as SdpWalletAdapterTrait>::new(
                OutboundRelay::new(sender),
            );
        let operation = tokio::spawn(operation(adapter, config.clone()));

        assert_funding_policy(&mut receiver, config.max_tx_fee).await;
        operation
            .await
            .expect("SDP operation should finish after wallet response");
    }

    #[tokio::test]
    async fn all_sdp_operations_use_twelve_percent_priority_and_one_and_a_half_hour_horizon() {
        let config = test_config();

        assert_operation(
            async |adapter, config| {
                adapter
                    .declare_tx(MantleTxBuilder::new(), declaration(), &config)
                    .await
            },
            &config,
        )
        .await;
        assert_operation(
            async |adapter, config| {
                adapter
                    .active_tx(MantleTxBuilder::new(), active(), &config)
                    .await
            },
            &config,
        )
        .await;
        assert_operation(
            async |adapter, config| {
                adapter
                    .withdraw_tx(MantleTxBuilder::new(), withdraw(), &config)
                    .await
            },
            &config,
        )
        .await;
    }
}
