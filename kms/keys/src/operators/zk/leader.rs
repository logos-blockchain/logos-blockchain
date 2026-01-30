// TODO: Make this secure by embedding actual logic
//       once resolving cyclic dep: kms-keys <> core
#[cfg(feature = "unsafe")]
pub mod unsafe_leader_key_operator {
    use std::fmt::{self, Debug, Formatter};

    use tokio::sync::oneshot;

    use crate::keys::{
        UnsecuredZkKey, ZkKey,
        secured_key::{SecureKeyOperator, SecuredKey},
    };

    pub struct UnsafeLeaderKeyOperator {
        result_channel: oneshot::Sender<UnsecuredZkKey>,
    }

    impl UnsafeLeaderKeyOperator {
        #[must_use]
        pub const fn new(result_channel: oneshot::Sender<UnsecuredZkKey>) -> Self {
            Self { result_channel }
        }
    }

    #[async_trait::async_trait]
    impl SecureKeyOperator for UnsafeLeaderKeyOperator {
        type Key = ZkKey;
        type Error = <ZkKey as SecuredKey>::Error;

        async fn execute(self: Box<Self>, key: &Self::Key) -> Result<(), Self::Error> {
            if self
                .result_channel
                .send(key.clone().into_unsecured())
                .is_err()
            {
                tracing::error!("Failed to send result via channel");
            }
            Ok(())
        }
    }

    impl Debug for UnsafeLeaderKeyOperator {
        fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
            write!(f, "UnsafeLeaderKeyOperator")
        }
    }
}
