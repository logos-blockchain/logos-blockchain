mod runtime;

use std::path::Path;

use async_trait::async_trait;
use testing_framework_core::scenario::DynError;
use testing_framework_runner_compose::{
    ComposeDeployEnv, ComposeDescriptor, DockerConfigServerSpec,
};

use super::{LbcEnv, deployment_artifacts::add_shared_deployment_file};

#[async_trait]
impl ComposeDeployEnv for LbcEnv {
    fn compose_descriptor(topology: &Self::Deployment, cfgsync_port: u16) -> ComposeDescriptor {
        let cfgsync_port = runtime::normalized_cfgsync_port(cfgsync_port);
        let (image, platform) = runtime::resolve_node_image();
        let nodes = topology
            .nodes()
            .iter()
            .enumerate()
            .map(|(index, node)| {
                runtime::build_node_descriptor(index, node, cfgsync_port, &image, platform.clone())
            })
            .collect();

        ComposeDescriptor::new(nodes)
    }

    fn enrich_cfgsync_artifacts(
        topology: &Self::Deployment,
        artifacts: &mut testing_framework_core::cfgsync::MaterializedArtifacts,
    ) -> Result<(), DynError> {
        let hostnames = <Self as ComposeDeployEnv>::cfgsync_hostnames(topology);

        add_shared_deployment_file(topology, &hostnames, artifacts).map_err(Into::into)
    }

    fn cfgsync_container_spec(
        cfgsync_path: &Path,
        port: u16,
        network: &str,
    ) -> Result<DockerConfigServerSpec, DynError> {
        let testnet_dir = runtime::cfgsync_dir(cfgsync_path)?;
        let (image, platform) = runtime::resolve_bootstrap_image();
        let container_name = runtime::cfgsync_container_name();
        Ok(runtime::build_cfgsync_container_spec(
            &container_name,
            network,
            port,
            testnet_dir,
            &image,
            platform,
        ))
    }
}
