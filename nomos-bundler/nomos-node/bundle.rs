use std::{env::set_var, path::PathBuf};

use bundler::utils::{
    get_cargo_package_version, get_project_identifier, get_target_directory_for_current_profile,
    get_workspace_root,
};
use clap::Parser;
use dunce::canonicalize;
use log::{debug, error, info};
use tauri_bundler::{
    AppImageSettings, DebianSettings, DmgSettings, IosSettings, MacOsSettings, RpmSettings,
    WindowsSettings,
};
use tauri_utils::platform::target_triple;

const CRATE_NAME: &str = "nomos-node";
const CRATE_PATH_RELATIVE_TO_WORKSPACE_ROOT: &str = "nodes/nomos-node/node";

/// Prepares the environment for bundling the application
fn prepare_environment(architecture: &str) {
    // SAFETY: This is a single-threaded context as no threads have been spawned
    // yet. Due to this, it is safe to call `build_package` which internally
    // relies on `set_var`, a function that is unsafe in some multithreaded
    // contexts.
    unsafe {
        // Bypass an issue in the current `linuxdeploy`'s version
        set_var("NO_STRIP", "true");
    };

    // SAFETY: This is a single-threaded context as no threads have been spawned
    // yet. Due to this, it is safe to call `build_package` which internally
    // relies on `set_var`, a function that is unsafe in some multithreaded
    // contexts.
    unsafe {
        // Tell `appimagetool` what architecture we're building for.
        // Without this, the tool errors out.
        // This could be due to us making an ad-hoc use of `tauri-bundler` here. We
        // might be bypassing some `tauri-bundler` piece of code or config that
        // handles that. If that's the case, I couldn't find what that would be.
        // Regardless, this works around that issue.
        set_var("ARCH", architecture);
    };
}

/// Bundles the package
fn build_package(version: String) {
    let crate_path = get_workspace_root().join(CRATE_PATH_RELATIVE_TO_WORKSPACE_ROOT);
    info!("Bundling package '{}'", crate_path.display());
    let resources_path = crate_path.join("resources");
    let icons_path = resources_path.join("icons");

    // This simultaneously serves as input directory (where the binary is)
    // and output (where the bundle will be)
    let target_triple = target_triple().expect("Could not determine the target triple.");
    let project_target_directory = canonicalize(
        get_target_directory_for_current_profile(target_triple.as_str())
            .expect("Could not determine the target directory."),
    )
    .expect("Could not canonicalize the target directory path.");
    debug!(
        "Bundle output directory: '{}'",
        project_target_directory.display()
    );

    // Icons
    let icon = icons_path.join("icon.ico").to_string_lossy().to_string();
    let icon512 = icons_path.join("512x512.png").to_string_lossy().to_string();
    debug!("Using icon: '{icon}' and icon512: '{icon512}'");

    // Windows settings
    #[expect(
        deprecated,
        reason = "While using tauri-bundler<=3.0.0, we need to explicitly set Windows' icon. Otherwise, it'll use a default path in the repository root."
    )]
    let windows = WindowsSettings {
        icon_path: PathBuf::from(&icon),
        ..Default::default()
    };

    // Linux Settings
    // Any level of GZIP compression will make the binary building fail
    // TODO: Re-enable RPM compression when the issue is fixed
    let rpm_settings: RpmSettings = RpmSettings {
        compression: Some(tauri_utils::config::RpmCompression::None),
        ..Default::default()
    };

    // Building settings
    let settings_builder = tauri_bundler::SettingsBuilder::new()
        .log_level(log::Level::Error)
        .package_settings(tauri_bundler::PackageSettings {
            product_name: String::from(CRATE_NAME),
            version,
            description: "Nomos Node".to_owned(),
            homepage: None,
            authors: None,
            default_run: None,
        })
        .project_out_directory(&project_target_directory)
        .bundle_settings(tauri_bundler::BundleSettings {
            identifier: Some(get_project_identifier(CRATE_NAME)),
            publisher: None,
            homepage: None,
            icon: Some(vec![icon, icon512]),
            resources: None,
            resources_map: None,
            copyright: None,
            license: Some("MIT or Apache-2.0".to_owned()),
            license_file: None,
            category: None,
            file_associations: None,
            short_description: None,
            long_description: None,
            bin: None,
            external_bin: None,
            deep_link_protocols: None,
            deb: DebianSettings::default(),
            appimage: AppImageSettings::default(),
            rpm: rpm_settings,
            dmg: DmgSettings::default(),
            macos: MacOsSettings::default(),
            updater: None,
            windows,
            ios: IosSettings::default(),
        })
        .binaries(vec![tauri_bundler::BundleBinary::new(
            String::from(CRATE_NAME),
            true,
        )]);

    let settings = settings_builder
        .build()
        .expect("Error while building settings");

    let arch = settings
        .target()
        .split('-')
        .next()
        .expect("Could not determine target architecture.");
    info!("Bundling for '{arch}'");

    prepare_environment(arch);

    if let Err(error) = tauri_bundler::bundle_project(&settings) {
        error!("Error while bundling the project: {error:?}");
        std::process::exit(1);
    } else {
        info!("Package bundled successfully");
    }
}

#[derive(Parser)]
struct BundleArguments {
    #[arg(
        short,
        long,
        value_name = "VERSION",
        help = "Expected Cargo package version in X.Y.Z format. If passed, this verifies the Cargo package version, panicking if it doesn't match."
    )]
    version: Option<String>,
}

/// If a version argument is provided, verify it matches the Cargo package
/// version.
/// This is passed by the CI/CD pipeline to ensure the bundled version is
/// consistent.
fn parse_version(arguments: BundleArguments, cargo_package_version: String) -> String {
    if let Some(version) = arguments.version {
        // Check for version mismatch
        // Maybe this should be a warning instead of a panic?
        assert_eq!(
            version, cargo_package_version,
            "Error: Expected Cargo package version: '{cargo_package_version}', \
            but received argument: '{version}'. \
            Please ensure the version matches the Cargo package version."
        );

        version
    } else {
        cargo_package_version
    }
}

fn main() {
    let _ = env_logger::try_init();

    let cargo_package_version = get_cargo_package_version(CRATE_NAME);
    let bundle_arguments = BundleArguments::parse();
    let version = parse_version(bundle_arguments, cargo_package_version);

    build_package(version);
}
