use std::pin::Pin;

use futures::channel::oneshot;
use lb_da_messages::replication::ReplicationRequest;
use lb_subnetworks_assignations::MembershipHandler;
use log::{debug, error};
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    maintenance::monitor::{ConnectionMonitor, ConnectionMonitorBehaviour},
    protocols::{
        dispersal::validator::behaviour::DispersalEvent, replication::behaviour::ReplicationEvent,
        sampling::SamplingEvent,
    },
    swarm::{DispersalValidationError, DispersalValidatorEvent},
};

pub type ValidationTask =
    Pin<Box<dyn Future<Output = (Result<(), DispersalValidationError>, DispersalEvent)> + Send>>;

pub fn handle_validator_dispersal_event(
    validation_events_sender: &UnboundedSender<DispersalValidatorEvent>,
    event: DispersalEvent,
) -> Option<ValidationTask> {
    if matches!(event, DispersalEvent::DispersalError { .. }) {
        return None;
    }

    let (sender, receiver) = oneshot::channel();
    let validation_event = DispersalValidatorEvent {
        event: event.clone(),
        sender: Some(sender),
    };

    if let Err(e) = validation_events_sender.send(validation_event) {
        error!("Error sending blob to validation: {e:?}");
        return None;
    }

    let validation_future = async move {
        let result = (receiver.await).unwrap_or_else(|_| {
            error!("Dispersal validation channel closed unexpectedly.");
            Err(DispersalValidationError)
        });

        (result, event)
    };

    Some(Box::pin(validation_future))
}

pub fn handle_sampling_event(
    sampling_events_sender: &UnboundedSender<SamplingEvent>,
    event: SamplingEvent,
) {
    if let Err(e) = sampling_events_sender.send(event) {
        debug!("Error distributing sampling message internally: {e:?}");
    }
}

pub fn handle_replication_event<Membership>(
    validation_events_sender: &UnboundedSender<DispersalValidatorEvent>,
    membership: &Membership,
    peer_id: &<Membership as MembershipHandler>::Id,
    event: ReplicationEvent,
) where
    Membership: MembershipHandler + Send + Sync,
    <Membership as MembershipHandler>::Id: Send + Sync,
{
    if let ReplicationEvent::IncomingMessage { message, .. } = event {
        let dispersal_event = match *message {
            ReplicationRequest::Share(share_request) => DispersalEvent::from(share_request.share),
            ReplicationRequest::Tx(tx) => {
                let assignations = membership.membership(peer_id).len();
                DispersalEvent::from((assignations as u16, tx))
            }
        };

        let validation_event = DispersalValidatorEvent {
            event: dispersal_event,
            sender: None,
        };

        if let Err(e) = validation_events_sender.send(validation_event) {
            error!("Error sending blob to validation: {e:?}");
        }
    }
}

pub fn monitor_event<Monitor: ConnectionMonitor>(
    monitor_behaviour: &mut ConnectionMonitorBehaviour<Monitor>,
    event: Monitor::Event,
) {
    monitor_behaviour.record_event(event);
}
