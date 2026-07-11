use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct TokioConsoleProfileNode {
    pub port: u16,
}

#[derive(Debug, Clone, Default)]
pub struct TokioConsoleProfile {
    pub profile_nodes: BTreeMap<String, TokioConsoleProfileNode>,
}

impl TokioConsoleProfile {
    #[must_use]
    pub fn is_enabled_for(&self, node_name: &str) -> bool {
        self.profile_nodes.contains_key(node_name)
    }

    #[must_use]
    pub fn node(&self, node_name: &str) -> Option<&TokioConsoleProfileNode> {
        self.profile_nodes.get(node_name)
    }
}
