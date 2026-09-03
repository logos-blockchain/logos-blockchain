use std::fmt::{Debug, Display};

use lb_blend_service::message::{BlendPayload, NetworkInfo, ProxyServiceMessage, ServiceMessage};
use lb_core::codec::{DeserializeOp, SerializeOp};
use lb_network_service::backends::libp2p::PeerId;
use overwatch::services::{AsServiceId, ServiceData};
use tokio::sync::oneshot;

pub async fn blend_info<BlendService, RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
) -> Result<Option<NetworkInfo<PeerId>>, overwatch::DynError>
where
    BlendService: ServiceData<Message = ProxyServiceMessage<ServiceMessage<PeerId>>>,
    RuntimeServiceId: AsServiceId<BlendService> + Debug + Sync + Display + 'static,
{
    let relay = handle.relay::<BlendService>().await?;
    let (sender, receiver) = oneshot::channel();

    relay
        .send(ServiceMessage::GetNetworkInfo { reply: sender }.into())
        .await
        .map_err(|(e, _)| e)?;

    receiver
        .await
        .map_err(|e| Box::new(e) as overwatch::DynError)
}

pub async fn blend_join_network<BlendService, RuntimeServiceId>(
    handle: &overwatch::overwatch::OverwatchHandle<RuntimeServiceId>,
    locator: lb_core::sdp::Locator,
    service_note_id: lb_core::mantle::NoteId,
) -> Result<lb_core::sdp::DeclarationId, overwatch::DynError>
where
    BlendService: ServiceData<Message = ProxyServiceMessage<ServiceMessage<PeerId>>>,
    RuntimeServiceId: AsServiceId<BlendService> + Debug + Sync + Display + 'static,
{
    let relay = handle.relay::<BlendService>().await?;
    let (sender, receiver) = oneshot::channel();

    relay
        .send(ProxyServiceMessage::JoinAsCore {
            locator,
            service_note_id,
            reply: sender,
        })
        .await
        .map_err(|(e, _)| e)?;

    let result = receiver
        .await
        .map_err(|e| Box::new(e) as overwatch::DynError)??;

    Ok(result)
}

/// Sends a transaction through the Blend network without adding it to this
/// node's own mempool.
///
/// Returns the transaction's id. Getting one back means the transaction was
/// accepted for blending, not that it was sent.
pub async fn blend_transaction<BlendService, Transaction, Id, RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
    transaction: Transaction,
    id: impl Fn(&Transaction) -> Id,
) -> Result<Id, overwatch::DynError>
where
    BlendService: ServiceData<Message = ProxyServiceMessage<ServiceMessage<PeerId>>>,
    Transaction: SerializeOp,
    RuntimeServiceId: AsServiceId<BlendService> + Debug + Sync + Display + 'static,
{
    // Encoded the same way the mempool gossips transactions, so that whichever
    // node exits this one decodes what it expects.
    let payload = BlendPayload::try_from_transaction(&transaction)?;
    let relay = handle.relay::<BlendService>().await?;

    relay
        .send(ServiceMessage::Blend(payload).into())
        .await
        .map_err(|(e, _)| e)?;

    Ok(id(&transaction))
}

/// The ids of the transactions this node is still waiting on a `PoW` solution
/// for.
///
/// Only those: once a transaction has been encapsulated it is in the
/// scheduler's hands, waiting for a release round, and no longer reported here.
// TODO: Have Blend hand back transactions rather than bytes, so that this can
// report their ids without decoding them again.
pub async fn blend_pending_transactions<BlendService, Transaction, Id, RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
    id: impl Fn(&Transaction) -> Id,
) -> Result<Vec<Id>, overwatch::DynError>
where
    BlendService: ServiceData<Message = ProxyServiceMessage<ServiceMessage<PeerId>>>,
    Transaction: DeserializeOp,
    RuntimeServiceId: AsServiceId<BlendService> + Debug + Sync + Display + 'static,
{
    let relay = handle.relay::<BlendService>().await?;
    let (sender, receiver) = oneshot::channel();

    relay
        .send(ServiceMessage::GetPendingTransactions { reply: sender }.into())
        .await
        .map_err(|(e, _)| e)?;

    receiver
        .await
        .map_err(|e| Box::new(e) as overwatch::DynError)?
        .iter()
        .map(|encoded_transaction| {
            // These are transactions this node encoded itself on the way in, so
            // a decoding failure here is this node disagreeing with itself.
            Ok(id(&Transaction::from_bytes(encoded_transaction)?))
        })
        .collect()
}
