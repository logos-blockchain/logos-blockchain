use std::{
    env,
    path::{Path, PathBuf},
};

use anyhow::anyhow;
use testing_framework_core::scenario::DynError;
use testing_framework_runner_compose::{
    DockerConfigServerSpec, DockerPortBinding, DockerVolumeMount, EnvEntry, NodeDescriptor,
    host_gateway_entry, node_identifier, repository_root,
};
use tracing::debug;
use uuid::Uuid;

use crate::{
    env as tf_env,
    framework::{
        constants::DEFAULT_CFGSYNC_PORT,
        image::{resolve_compose_bootstrap_image, resolve_compose_node_image},
    },
    internal::NodePlan,
};

const NODE_ENTRYPOINT: &str = "/etc/logos/scripts/run_logos_node.sh";
const GHCR_TESTNET_IMAGE: &str = "ghcr.io/logos-co/nomos:testnet";
const DEFAULT_CFGSYNC_HOST: &str = "cfgsync";

pub(super) const fn normalized_cfgsync_port(port: u16) -> u16 {
    if port == 0 {
        DEFAULT_CFGSYNC_PORT
    } else {
        port
    }
}

pub(super) fn build_node_descriptor(
    index: usize,
    node: &NodePlan,
    cfgsync_port: u16,
    image: &str,
    platform: Option<String>,
) -> NodeDescriptor {
    let mut environment = base_environment(cfgsync_port);
    environment.push(EnvEntry::new("CFG_HOST_IDENTIFIER", node_identifier(index)));

    let api_port = node.general.api_config.address.port();
    let testing_port = node.general.api_config.testing_http_address.port();

    NodeDescriptor::with_loopback_ports(
        node_identifier(index),
        image.to_owned(),
        NODE_ENTRYPOINT,
        base_volumes(),
        default_extra_hosts(),
        vec![api_port, testing_port],
        environment,
        platform,
    )
}

pub(super) fn cfgsync_dir(cfgsync_path: &Path) -> Result<&Path, DynError> {
    cfgsync_path.parent().ok_or_else(|| {
        anyhow!(
            "cfgsync path {} has no parent directory",
            cfgsync_path.display()
        )
        .into()
    })
}

pub(super) fn cfgsync_container_name() -> String {
    format!("nomos-cfgsync-{}", Uuid::new_v4())
}

pub(super) fn build_cfgsync_container_spec(
    container_name: &str,
    network: &str,
    port: u16,
    testnet_dir: &Path,
    image: &str,
    platform: Option<String>,
) -> DockerConfigServerSpec {
    let mut mounts = vec![DockerVolumeMount::read_only(
        testnet_dir.to_path_buf(),
        "/etc/logos".to_owned(),
    )];
    let mut env = Vec::new();

    maybe_add_circuits_mount(&mut mounts, &mut env);

    DockerConfigServerSpec::new(
        container_name.to_owned(),
        network.to_owned(),
        "cfgsync-server".to_owned(),
        image.to_owned(),
    )
    .with_platform(platform)
    .with_network_alias("cfgsync".to_owned())
    .with_workdir("/etc/logos".to_owned())
    .with_ports(vec![DockerPortBinding::tcp(port, port)])
    .with_mounts(mounts)
    .with_env(env)
    .with_args(vec!["/etc/logos/cfgsync.yaml".to_owned()])
}

pub(super) fn resolve_node_image() -> (String, Option<String>) {
    let image = resolve_compose_node_image().name;
    let platform = image_platform(&image);

    debug!(image, platform = ?platform, "resolved compose image");

    (image, platform)
}

pub(super) fn resolve_bootstrap_image() -> (String, Option<String>) {
    let image = resolve_compose_bootstrap_image().name;
    let platform = image_platform(&image);

    debug!(image, platform = ?platform, "resolved compose bootstrap image");

    (image, platform)
}

fn maybe_add_circuits_mount(mounts: &mut Vec<DockerVolumeMount>, env: &mut Vec<(String, String)>) {
    let circuits_dir = env::var("LOGOS_BLOCKCHAIN_CIRCUITS_DOCKER")
        .ok()
        .or_else(|| env::var("LOGOS_BLOCKCHAIN_CIRCUITS").ok());

    let Some(circuits_dir) = circuits_dir else {
        return;
    };

    let host_path = PathBuf::from(&circuits_dir);
    if !host_path.exists() {
        return;
    }

    let resolved_host_path = host_path.canonicalize().unwrap_or(host_path);
    env.push(("LOGOS_BLOCKCHAIN_CIRCUITS".to_owned(), circuits_dir.clone()));

    mounts.push(DockerVolumeMount::read_only(
        resolved_host_path,
        circuits_dir,
    ));
}

fn base_volumes() -> Vec<String> {
    let mut volumes = vec!["./stack:/etc/logos".into()];
    if let Some(host_log_dir) = repository_root()
        .ok()
        .map(|root| root.join("tmp").join("node-logs"))
        .map(|dir| dir.display().to_string())
    {
        volumes.push(format!("{host_log_dir}:/tmp/node-logs"));
    }
    volumes
}

fn default_extra_hosts() -> Vec<String> {
    host_gateway_entry().into_iter().collect()
}

fn base_environment(cfgsync_port: u16) -> Vec<EnvEntry> {
    let rust_log = env_value_or_default(tf_env::rust_log, "info");
    let nomos_log_level = env_value_or_default(tf_env::nomos_log_level, "info");
    let time_backend = env_value_or_default(tf_env::lb_time_service_backend, "monotonic");
    let cfgsync_host = env::var("LOGOS_BLOCKCHAIN_CFGSYNC_HOST")
        .unwrap_or_else(|_| String::from(DEFAULT_CFGSYNC_HOST));

    vec![
        EnvEntry::new("RUST_LOG", rust_log),
        EnvEntry::new("LOGOS_BLOCKCHAIN_LOG_LEVEL", nomos_log_level),
        EnvEntry::new("LOGOS_BLOCKCHAIN_TIME_BACKEND", time_backend),
        EnvEntry::new(
            "CFG_SERVER_ADDR",
            format!("http://{cfgsync_host}:{cfgsync_port}"),
        ),
        EnvEntry::new("OTEL_METRIC_EXPORT_INTERVAL", "5000"),
    ]
}

fn env_value_or_default(getter: impl Fn() -> Option<String>, default: &'static str) -> String {
    getter().unwrap_or_else(|| String::from(default))
}

fn image_platform(image: &str) -> Option<String> {
    (image == GHCR_TESTNET_IMAGE).then(|| "linux/amd64".to_owned())
}
