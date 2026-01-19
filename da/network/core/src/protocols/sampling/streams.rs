use futures::AsyncWriteExt as _;
use lb_da_messages::{
    packing::{pack_to_writer, unpack_from_reader},
    sampling::{self, SampleResponse},
};
use libp2p::{PeerId, Stream};

use super::{
    BehaviourSampleReq, ResponseChannel, SampleRequestFutureSuccess, errors::SamplingError,
};
use crate::protocols::sampling::SampleRequestFutureError;

type StreamSampleResponse = (PeerId, SampleResponse, SampleStream);

/// Auxiliary struct that binds a stream with the corresponding `PeerId`
#[derive(Debug)]
pub struct SampleStream {
    pub stream: Stream,
    pub peer_id: PeerId,
}

/// Task for handling streams, one message at a time
/// Writes the request to the stream and waits for a response
pub async fn stream_sample(
    mut stream: SampleStream,
    message: sampling::SampleRequest,
) -> Result<StreamSampleResponse, SampleRequestFutureError> {
    let peer_id = stream.peer_id;
    if let Err(error) = pack_to_writer(&message, &mut stream.stream).await {
        return Err((
            SamplingError::Io {
                peer_id,
                error,
                message: Some(message),
            },
            Some(stream),
        ));
    }

    if let Err(error) = stream.stream.flush().await {
        return Err((
            SamplingError::Io {
                peer_id,
                error,
                message: Some(message),
            },
            Some(stream),
        ));
    }

    let response = match unpack_from_reader(&mut stream.stream).await {
        Ok(response) => response,
        Err(error) => {
            return Err((
                SamplingError::Io {
                    peer_id,
                    error,
                    message: Some(message),
                },
                Some(stream),
            ));
        }
    };

    Ok((peer_id, response, stream))
}

/// Handler for incoming streams
/// Pulls a request from the stream and replies if possible
pub async fn handle_incoming_stream(
    mut stream: SampleStream,
    channel: ResponseChannel,
) -> Result<SampleRequestFutureSuccess, SampleRequestFutureError> {
    let peer_id = stream.peer_id;
    let request: sampling::SampleRequest = match unpack_from_reader(&mut stream.stream).await {
        Ok(req) => req,
        Err(error) => {
            return Err((
                SamplingError::Io {
                    peer_id,
                    error,
                    message: None,
                },
                Some(stream),
            ));
        }
    };

    let request = match BehaviourSampleReq::try_from(request) {
        Ok(req) => req,
        Err(blob_id) => {
            return Err((
                SamplingError::InvalidBlobId { peer_id, blob_id },
                Some(stream),
            ));
        }
    };

    if let Err(request) = channel.request_sender.send(request) {
        return Err((
            SamplingError::RequestChannel { request, peer_id },
            Some(stream),
        ));
    }

    let response: SampleResponse = match channel.response_receiver.await {
        Ok(resp) => resp.into(),
        Err(error) => {
            return Err((
                SamplingError::ResponseChannel { error, peer_id },
                Some(stream),
            ));
        }
    };

    if let Err(error) = pack_to_writer(&response, &mut stream.stream).await {
        return Err((
            SamplingError::Io {
                peer_id,
                error,
                message: None,
            },
            Some(stream),
        ));
    }

    if let Err(error) = stream.stream.flush().await {
        return Err((
            SamplingError::Io {
                peer_id,
                error,
                message: None,
            },
            Some(stream),
        ));
    }

    Ok((peer_id, stream))
}
