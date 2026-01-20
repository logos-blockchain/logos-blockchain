#[async_trait::async_trait]
pub trait MempoolVerificationProvider {
    type Payload;
    type Parameters;
    type Settings: Clone;

    // TODO: Payload verification parameters most likely will come from another
    // Overwatch service. Once it's decided, update the `new` method to require
    // service relay as parameter.
    fn new(settings: Self::Settings) -> Self;

    async fn get_parameters(&self, payload: &Self::Payload) -> Self::Parameters;
}
