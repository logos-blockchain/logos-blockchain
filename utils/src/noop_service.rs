use overwatch::{
    OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};

pub struct NoService;

impl ServiceData for NoService {
    type Settings = ();
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = ();
}

#[async_trait::async_trait]
impl<RuntimeServiceId> ServiceCore<RuntimeServiceId> for NoService
where
    RuntimeServiceId: AsServiceId<Self>,
{
    fn init(
        _: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _: Self::State,
    ) -> Result<Self, overwatch::DynError> {
        Ok(Self)
    }
    async fn run(self) -> Result<(), overwatch::DynError> {
        Ok(())
    }
}
