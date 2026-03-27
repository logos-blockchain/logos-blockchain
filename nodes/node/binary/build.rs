use std::{env, ffi::OsStr, process::Command};

fn run_command<Cmd, ArgIter, Arg>(command: Cmd, args: ArgIter) -> Option<String>
where
    Cmd: AsRef<OsStr>,
    ArgIter: IntoIterator<Item = Arg>,
    Arg: AsRef<OsStr>,
{
    Command::new(command)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn get_head_commit_hash() -> Option<String> {
    run_command("git", ["rev-parse", "--short", "HEAD"])
}

fn get_head_tag_name() -> Option<String> {
    run_command("git", ["describe", "--tags", "--exact-match", "HEAD"])
}

fn get_rustc_version() -> String {
    let rustc_binary = env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    run_command(rustc_binary, ["--version"])
        .expect("Rustc binary should always be available when compiling a Rust project.")
}

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");

    let head_commit_hash = get_head_commit_hash().unwrap_or_default();
    println!("cargo:rustc-env=HEAD_COMMIT_HASH={head_commit_hash}");

    let head_tag_name = get_head_tag_name().unwrap_or_default();
    println!("cargo:rustc-env=HEAD_TAG_NAME={head_tag_name}");

    let rustc_version = get_rustc_version();
    println!("cargo:rustc-env=RUSTC_VERSION={rustc_version}");

    // These env variables are injected by Cargo.
    let pkg_version = env::var("CARGO_PKG_VERSION").unwrap();
    println!("cargo:rustc-env=PKG_VERSION={pkg_version}");

    let profile = env::var("PROFILE").unwrap();
    println!("cargo:rustc-env=PROFILE={profile}");

    let target = env::var("TARGET").unwrap();
    println!("cargo:rustc-env=TARGET={target}");
}
