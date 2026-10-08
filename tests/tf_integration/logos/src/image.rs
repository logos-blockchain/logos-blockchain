use std::env;

use crate::env as tf_env;

const DEFAULT_LOCAL_COMPOSE_NODE_IMAGE: &str = "logos-blockchain-node-testing:local";
const DEFAULT_LOCAL_COMPOSE_BOOTSTRAP_IMAGE: &str = "logos-blockchain-cfgsync-testing:local";
const DEFAULT_LOCAL_K8S_NODE_IMAGE: &str = "logos-blockchain-node-testing:local";
const DEFAULT_LOCAL_K8S_BOOTSTRAP_IMAGE: &str = "logos-blockchain-cfgsync-testing:local";
const LOGOS_COMPOSE_NODE_IMAGE: &str = "LOGOS_BLOCKCHAIN_COMPOSE_NODE_IMAGE";
const LOGOS_COMPOSE_BOOTSTRAP_IMAGE: &str = "LOGOS_BLOCKCHAIN_COMPOSE_BOOTSTRAP_IMAGE";
const LOGOS_K8S_NODE_IMAGE: &str = "LOGOS_BLOCKCHAIN_K8S_NODE_IMAGE";
const LOGOS_K8S_BOOTSTRAP_IMAGE: &str = "LOGOS_BLOCKCHAIN_K8S_BOOTSTRAP_IMAGE";

#[derive(Clone, Debug)]
pub struct ResolvedImage {
    pub name: String,
    pub local: bool,
}

pub fn resolve_k8s_node_image() -> ResolvedImage {
    resolve_runner_image(LOGOS_K8S_NODE_IMAGE, DEFAULT_LOCAL_K8S_NODE_IMAGE)
}

pub fn resolve_k8s_bootstrap_image() -> ResolvedImage {
    resolve_runner_image(LOGOS_K8S_BOOTSTRAP_IMAGE, DEFAULT_LOCAL_K8S_BOOTSTRAP_IMAGE)
}

pub fn resolve_compose_node_image() -> ResolvedImage {
    resolve_runner_image(LOGOS_COMPOSE_NODE_IMAGE, DEFAULT_LOCAL_COMPOSE_NODE_IMAGE)
}

pub fn resolve_compose_bootstrap_image() -> ResolvedImage {
    resolve_runner_image(
        LOGOS_COMPOSE_BOOTSTRAP_IMAGE,
        DEFAULT_LOCAL_COMPOSE_BOOTSTRAP_IMAGE,
    )
}

fn resolve_named_image(key: &str) -> Option<ResolvedImage> {
    env::var(key).ok().map(resolved_remote_image)
}

fn resolve_runner_image(key: &str, default_local_image: &str) -> ResolvedImage {
    resolve_named_image(key)
        .or_else(|| tf_env::logos_blockchain_testnet_image().map(resolved_remote_image))
        .unwrap_or_else(|| resolved_local_image(default_local_image))
}

fn resolved_local_image(name: &str) -> ResolvedImage {
    ResolvedImage {
        name: name.to_owned(),
        local: true,
    }
}

const fn resolved_remote_image(name: String) -> ResolvedImage {
    ResolvedImage { name, local: false }
}
