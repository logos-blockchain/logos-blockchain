use serde::{Deserialize, Serialize};

pub mod dispersal;
pub mod wallet;

/// A no-operation body for endpoints that do not require a request or response
/// body.
///
/// This is a workaround used to satisfy the type system in niche cases that
/// should be addressed in the future. E.g.: A `get` requiring a request body.
#[derive(Serialize, Deserialize)]
pub struct NoopBody;
