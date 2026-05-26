use std::collections::HashMap;

use lb_key_management_system_service::keys::Key;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Keystore {
    keys: HashMap<String, Key>,
    mapping: HashMap<String, String>,
}
