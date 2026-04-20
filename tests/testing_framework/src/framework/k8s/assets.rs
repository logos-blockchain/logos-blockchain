use std::{
    env, io,
    path::{Path, PathBuf},
};

use anyhow::Result as AnyhowResult;
use reqwest::Url;
use tempfile::TempDir;
use testing_framework_core::cfgsync::{
    CfgsyncOutputPaths, RegistrationServerRenderOptions, render_and_write_registration_server,
};
use testing_framework_runner_k8s::{
    HelmReleaseAssets, HelmReleaseBundle, RequiredPathError, RuntimeSpecError,
    bundled_runner_chart_path, create_temp_workspace, require_existing_paths,
    resolve_optional_relative_dir, resolve_workspace_root,
};
use thiserror::Error;
use tracing::debug;

use super::{K8S_FULLNAME_OVERRIDE, build_runtime_spec};
use crate::{
    framework::{
        LbcEnv,
        constants::{DEFAULT_ASSETS_STACK_DIR, cfgsync_port},
        deployment_artifacts::add_shared_deployment_file,
    },
    internal::DeploymentPlan,
};

pub struct K8sAssets {
    pub release_bundle: HelmReleaseBundle,
    _tempdir: TempDir,
}

#[derive(Debug, Error)]
pub enum AssetsError {
    #[error("failed to locate workspace root: {source}")]
    WorkspaceRoot {
        #[source]
        source: anyhow::Error,
    },
    #[error("failed to render cfgsync configuration: {source}")]
    Cfgsync {
        #[source]
        source: anyhow::Error,
    },
    #[error("missing Helm chart at {path}; ensure the repository is up-to-date")]
    MissingChart { path: PathBuf },
    #[error(transparent)]
    RequiredPath(#[from] RequiredPathError),
    #[error("failed to create temporary directory for rendered assets: {source}")]
    TempDir {
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    RuntimeSpec(#[from] RuntimeSpecError),
}

impl HelmReleaseAssets for K8sAssets {
    fn release_bundle(&self) -> HelmReleaseBundle {
        self.release_bundle.clone()
    }
}

pub(super) struct ScriptPaths {
    pub(super) run_cfgsync: PathBuf,
    pub(super) run_shared: PathBuf,
    pub(super) run_node: PathBuf,
}

pub(super) fn prepare_assets(
    topology: &DeploymentPlan,
    _metrics_otlp_ingest_url: Option<&Url>,
) -> Result<K8sAssets, AssetsError> {
    let root = workspace_root().map_err(|source| AssetsError::WorkspaceRoot { source })?;
    let tempdir = create_assets_tempdir()?;
    let (cfgsync_file, cfgsync_yaml, artifacts_yaml) =
        render_and_write_cfgsync(topology, &tempdir)?;
    let scripts = validate_scripts(&root)?;
    let chart_path = helm_chart_path(&root)?;
    let runtime_spec = build_runtime_spec(
        topology,
        &cfgsync_yaml,
        &artifacts_yaml,
        cfgsync_file.clone(),
        scripts,
        chart_path.clone(),
    );
    let values_file = runtime_spec.write_values_file(tempdir.path(), "values.yaml")?;
    let mut release_bundle = runtime_spec.release_bundle();
    release_bundle.values_files = vec![values_file.clone()];

    debug!(
        cfgsync = %cfgsync_file.display(),
        values = %values_file.display(),
        node_image = runtime_spec.node_image,
        bootstrap_image = runtime_spec.shared_service.as_ref().map(|service| service.image.as_str()),
        chart = %chart_path.display(),
        "k8s runner assets prepared"
    );

    Ok(K8sAssets {
        release_bundle,
        _tempdir: tempdir,
    })
}

pub(super) fn workspace_root() -> AnyhowResult<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

    resolve_workspace_root(manifest_dir, "CARGO_WORKSPACE_DIR", |candidate| {
        let stack_root = resolve_optional_relative_dir(candidate, "REL_ASSETS_STACK_DIR")
            .unwrap_or_else(|| candidate.join(DEFAULT_ASSETS_STACK_DIR));
        stack_root.exists()
    })
}

fn create_assets_tempdir() -> Result<TempDir, AssetsError> {
    create_temp_workspace("nomos-helm-").map_err(|source| AssetsError::TempDir { source })
}

fn render_and_write_cfgsync(
    topology: &DeploymentPlan,
    tempdir: &TempDir,
) -> Result<(PathBuf, String, String), AssetsError> {
    let cfgsync_file = tempdir.path().join("cfgsync.yaml");
    let artifacts_file = tempdir.path().join("cfgsync.artifacts.yaml");
    let hostnames = k8s_node_hostnames(topology);
    let rendered = render_and_write_registration_server::<LbcEnv, _>(
        topology,
        &hostnames,
        RegistrationServerRenderOptions {
            port: Some(cfgsync_port()),
            artifacts_path: Some("cfgsync.artifacts.yaml".to_owned()),
        },
        CfgsyncOutputPaths {
            config_path: &cfgsync_file,
            artifacts_path: &artifacts_file,
        },
        |artifacts| add_shared_deployment_file(topology, &hostnames, artifacts).map_err(Into::into),
    )
    .map_err(|source| AssetsError::Cfgsync {
        source: source.into(),
    })?;

    write_entropy_file(&cfgsync_file).map_err(|source| AssetsError::Cfgsync { source })?;

    Ok((cfgsync_file, rendered.config_yaml, rendered.artifacts_yaml))
}

fn k8s_node_hostnames(topology: &DeploymentPlan) -> Vec<String> {
    topology
        .nodes()
        .iter()
        .map(|node| format!("{K8S_FULLNAME_OVERRIDE}-node-{}", node.index()))
        .collect()
}

fn validate_scripts(root: &Path) -> Result<ScriptPaths, AssetsError> {
    let scripts_dir = stack_scripts_root(root);
    let run_cfgsync = scripts_dir.join("run_cfgsync.sh");
    let run_shared = scripts_dir.join("run_logos.sh");
    let run_node = scripts_dir.join("run_logos_node.sh");

    drop(require_existing_paths([
        run_cfgsync.clone(),
        run_shared.clone(),
        run_node.clone(),
    ])?);

    Ok(ScriptPaths {
        run_cfgsync,
        run_shared,
        run_node,
    })
}

fn helm_chart_path(root: &Path) -> Result<PathBuf, AssetsError> {
    let path = resolve_optional_relative_dir(root, "REL_HELM_CHART_DIR")
        .unwrap_or_else(|| bundled_runner_chart_path(root));
    if path.exists() {
        Ok(path)
    } else {
        Err(AssetsError::MissingChart { path })
    }
}

fn stack_scripts_root(root: &Path) -> PathBuf {
    resolve_optional_relative_dir(root, "REL_ASSETS_STACK_DIR")
        .unwrap_or_else(|| root.join(DEFAULT_ASSETS_STACK_DIR))
        .join("scripts")
}

fn write_entropy_file(config_path: &Path) -> anyhow::Result<()> {
    let Some(dir) = config_path.parent() else {
        return Ok(());
    };

    std::fs::write(
        dir.join("cfgsync.entropy"),
        b"nomos-testing-framework-cfgsync-entropy",
    )?;

    Ok(())
}
