use std::{
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

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

fn find_repo_git_dir() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").ok()?);

    for dir in manifest_dir.ancestors() {
        let dot_git = dir.join(".git");

        if dot_git.is_dir() {
            return Some(dot_git);
        }

        if dot_git.is_file() {
            let contents = fs::read_to_string(&dot_git).ok()?;
            let gitdir = contents.strip_prefix("gitdir:")?.trim();
            let gitdir = PathBuf::from(gitdir);

            return Some(if gitdir.is_absolute() {
                gitdir
            } else {
                dir.join(gitdir)
            });
        }
    }

    None
}

fn emit_rerun_if_changed(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.to_string_lossy());
}

fn emit_git_watchers(git_dir: &Path) {
    // Watch the HEAD file, this changes when the current branch changes or when a
    // new commit is made on a detached HEAD.
    let head_path = git_dir.join("HEAD");
    emit_rerun_if_changed(&head_path);

    // Covers packed branch refs and packed tags.
    emit_rerun_if_changed(&git_dir.join("packed-refs"));

    // If HEAD points to a branch ref (e.g. refs/heads/master), watch only that
    // ref., this changes when the branch advances.
    if let Ok(head_contents) = fs::read_to_string(&head_path)
        && let Some(reference) = head_contents.strip_prefix("ref: ")
    {
        emit_rerun_if_changed(&git_dir.join(reference.trim()));
    }

    // Watch loose tags because HEAD_TAG_NAME depends on exact tags at HEAD.
    // This is broader than watching only current tags, but tag changes are rare
    // and it preserves correctness if a tag is created after a previous build.
    emit_rerun_if_changed(&git_dir.join("refs").join("tags"));
}

fn main() {
    emit_git_watchers(&find_repo_git_dir().expect("No git dir"));

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
