use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AddressMapperError {
    #[error("Failed to discover gateway: {0}")]
    GatewayDiscoveryFailed(String),

    #[error("Failed to get external IP: {0}")]
    ExternalIpFailed(String),

    #[error("Failed to add port mapping: {0}")]
    PortMappingFailed(String),

    #[error("No IP address found in multiaddr")]
    NoIpAddress,

    #[error("Channel send error")]
    ChannelSendError,

    #[error("Multiaddr parse error: {0}")]
    MultiaddrParseError(String),

    #[error("Mapping already in progress")]
    MappingAlreadyInProgress,
}

impl From<igd_next::SearchError> for AddressMapperError {
    fn from(error: igd_next::SearchError) -> Self {
        Self::GatewayDiscoveryFailed(error.to_string())
    }
}

impl From<igd_next::GetExternalIpError> for AddressMapperError {
    fn from(error: igd_next::GetExternalIpError) -> Self {
        Self::ExternalIpFailed(error.to_string())
    }
}

impl From<igd_next::AddPortError> for AddressMapperError {
    fn from(error: igd_next::AddPortError) -> Self {
        Self::PortMappingFailed(error.to_string())
    }
}

impl From<futures::channel::mpsc::SendError> for AddressMapperError {
    fn from(_: futures::channel::mpsc::SendError) -> Self {
        Self::ChannelSendError
    }
}

impl From<multiaddr::Error> for AddressMapperError {
    fn from(error: multiaddr::Error) -> Self {
        Self::MultiaddrParseError(error.to_string())
    }
}
