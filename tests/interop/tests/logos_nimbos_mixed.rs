//! Deploy two Logos nodes and two Nimbos nodes as one TF application.

use std::{env, path::PathBuf, time::Duration};

use async_trait::async_trait;
use blockchain_test_interop::nimbos::NimbosEnv;
use lb_testing_framework::{
    DeploymentBuilder, LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_URL, LbcEnv, NodeHttpClient,
    SharedDeployment, TopologyConfig,
};
use libp2p::{Multiaddr, PeerId, multiaddr::Protocol};
use testing_framework_app::{AppDeployer, AppDeployment, AppHostEnv, ClusterApp, DeployContext};
use testing_framework_core::scenario::{
    Application, ClusterControlRequest, ClusterHandle, ClusterRequest, DynError,
};
use tokio::time::{sleep, timeout};

/// Check both Nimbos peers from Logos, then stop one and check the survivor.
///
/// Set `NIMBOS_NODE_BIN` and `NIMBOS_CIRCUITS_DIR`. Select Logos using
/// `LOGOS_BLOCKCHAIN_NODE_BIN` or `LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_URL`; the
/// existing binary provider downloads and extracts release archives.
/// The current checkout generates configuration, so select a compatible Logos
/// binary. Nimbos preparation rejects settings it cannot represent faithfully.
///
/// ```text
/// cargo test -p blockchain-test-interop \
///   --test logos_nimbos_mixed -- --ignored
/// ```
#[tokio::test]
#[ignore = "requires a Logos binary path or release URL, NIMBOS_NODE_BIN and NIMBOS_CIRCUITS_DIR"]
async fn nimbos_nodes_connect_to_logos_nodes() -> Result<(), DynError> {
    // Require an explicit selection so this test cannot trigger an unexpected
    // build.
    let binary_selected = [
        "LOGOS_BLOCKCHAIN_NODE_BIN",
        LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_URL,
    ]
    .iter()
    .any(|name| env::var_os(name).is_some_and(|value| !value.is_empty()));
    if !binary_selected {
        return Err("set LOGOS_BLOCKCHAIN_NODE_BIN or LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_URL".into());
    }

    let dir = tempfile::tempdir()?;
    let plan = DeploymentBuilder::new(TopologyConfig::with_node_numbers(2))
        .scenario_base_dir(dir.path().join("logos"))
        .build()?;
    let shared = SharedDeployment::from_plan(&plan)?;
    let logos = ClusterApp::<LbcEnv>::new(plan);

    check_mixed_cluster(logos, shared).await
}

async fn check_mixed_cluster<A, E>(
    logos_app: A,
    shared_deployment: SharedDeployment,
) -> Result<(), DynError>
where
    E: Application<NodeClient = NodeHttpClient> + Clone,
    A: AppDeployment<AppHostEnv, Handle = ClusterHandle<E>>,
{
    let app = MixedClusterApp {
        logos_app,
        shared_deployment,
        nimbos_binary: PathBuf::from(env::var("NIMBOS_NODE_BIN")?),
        circuits_dir: PathBuf::from(env::var("NIMBOS_CIRCUITS_DIR")?),
    };
    let deployed = AppDeployer::new().deploy(app).await?;

    let cluster = deployed.handle();
    let logos = cluster.logos.clients();
    assert_eq!(logos.len(), 2, "expected two Logos nodes");

    let nimbos = cluster
        .nimbos
        .deployment()
        .ok_or("missing Nimbos deployment")?;
    let peer_ids = [nimbos.peer_id(0)?, nimbos.peer_id(1)?];

    // Nimbos network/info is currently a stub; observe the connection from Logos.
    check_peer_connections(&logos, &peer_ids).await?;

    let names = cluster.nimbos.node_names();
    let [stopped_node, remaining_node] = names.as_slice() else {
        return Err("expected two Nimbos nodes".into());
    };

    for name in &names {
        cluster
            .nimbos
            .node_pid(name)
            .ok_or_else(|| format!("{name} exited before the shutdown check"))?;
    }

    cluster.nimbos.stop_node(stopped_node).await?;

    check_peer_disconnection(&logos, &peer_ids[0]).await?;
    check_peer_connections(&logos, &peer_ids[1..]).await?;

    cluster
        .nimbos
        .node_pid(remaining_node)
        .ok_or("the remaining Nimbos node exited after its peer stopped")?;

    Ok(())
}

/// Prepare both child deployments from one generated network definition.
///
/// Each adapter uses the shared genesis and chain parameters to produce its
/// native configuration before either cluster is started.
#[derive(Clone)]
struct MixedClusterApp<A> {
    logos_app: A,
    shared_deployment: SharedDeployment,
    nimbos_binary: PathBuf,
    circuits_dir: PathBuf,
}

/// TF handles for observing and controlling the two child clusters.
///
/// Keep the owner returned by [`AppDeployer`] alive while using these handles.
/// Dropping that owner cleans up both clusters, including on test failure.
#[derive(Clone)]
struct MixedCluster<E: Application> {
    logos: ClusterHandle<E>,
    nimbos: ClusterHandle<NimbosEnv>,
}

#[async_trait]
impl<A, E> AppDeployment<AppHostEnv> for MixedClusterApp<A>
where
    E: Application<NodeClient = NodeHttpClient> + Clone,
    A: AppDeployment<AppHostEnv, Handle = ClusterHandle<E>>,
{
    type Handle = MixedCluster<E>;

    /// Deploy through [`DeployContext`], starting Logos first so Nimbos can
    /// bootstrap from its live addresses and peer IDs.
    async fn deploy(self, ctx: &mut DeployContext<AppHostEnv>) -> Result<Self::Handle, DynError> {
        let nimbos_deployment = NimbosEnv::prepare_deployment(
            &self.shared_deployment,
            &self.nimbos_binary,
            &self.circuits_dir,
        )?;

        let logos = ctx.deploy(self.logos_app).await?;

        let mut peers = Vec::new();
        for client in logos.clients() {
            let info = client.network_info().await?;
            let port = info
                .listen_addresses
                .iter()
                .find_map(|address| {
                    address.iter().find_map(|part| match part {
                        Protocol::Udp(port) => Some(port),
                        _ => None,
                    })
                })
                .ok_or("Logos node has no QUIC listener")?;
            let peer: Multiaddr =
                format!("/ip4/127.0.0.1/udp/{port}/quic-v1/p2p/{}", info.peer_id).parse()?;
            peers.push(peer);
        }

        let nimbos_deployment = nimbos_deployment.with_bootstrap_peers(peers)?;
        let nimbos = ctx
            .deploy_cluster(
                ClusterRequest::<NimbosEnv>::managed(nimbos_deployment)
                    .with_control(ClusterControlRequest::Full),
            )
            .await?;

        Ok(MixedCluster { logos, nimbos })
    }
}

/// Require every Logos node to report all expected Nimbos peers in two polls.
async fn check_peer_connections(
    logos: &[NodeHttpClient],
    peer_ids: &[PeerId],
) -> Result<(), DynError> {
    timeout(Duration::from_secs(30), async {
        let mut consecutive_polls = 0;

        loop {
            let mut all_connected = true;
            for client in logos {
                let info = client.network_info().await?;
                all_connected &= peer_ids.iter().all(|id| info.connected_peers.contains(id));
            }

            if all_connected {
                consecutive_polls += 1;
                if consecutive_polls == 2 {
                    return Ok::<_, DynError>(());
                }
            } else {
                consecutive_polls = 0;
            }

            sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .map_err(|_| "Nimbos peers did not remain connected to every Logos node within 30 seconds")??;

    Ok(())
}

/// Wait until every Logos node has removed the stopped peer from its view.
async fn check_peer_disconnection(
    logos: &[NodeHttpClient],
    peer_id: &PeerId,
) -> Result<(), DynError> {
    timeout(Duration::from_secs(30), async {
        loop {
            let mut all_disconnected = true;
            for client in logos {
                let info = client.network_info().await?;
                all_disconnected &= !info.connected_peers.contains(peer_id);
            }

            if all_disconnected {
                return Ok::<_, DynError>(());
            }

            sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .map_err(|_| "a Logos node still reports the stopped Nimbos peer after shutdown")??;

    Ok(())
}
