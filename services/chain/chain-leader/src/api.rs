use overwatch::services::ServiceData;

use crate::LeaderMsg;

pub trait ChainLeaderServiceData: ServiceData<Message = LeaderMsg> + Send + 'static {}

impl<T> ChainLeaderServiceData for T where T: ServiceData<Message = LeaderMsg> + Send + 'static {}
