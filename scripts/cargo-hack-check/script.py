#!/usr/bin/env python3
# Runs a cache-aware Cargo Hack check for all crates in the workspace.
#
# Each crate's cache key is a Merkle hash over:
# - Global inputs: root manifest, toolchain, cargo config, tool versions, check command, warnings policy.
# - The crate's own files (tracked and untracked-but-not-ignored).
# - The external packages it (transitively) depends on, as resolved in `cargo metadata`.
# - The cache keys of the workspace crates it depends on.
# A crate is skipped when its current key matches the key saved after its last successful check.
#
# Improvement: Dev-dependencies are not part of the key yet, although the check runs with `--all-targets`.


import json
import dataclasses
import argparse
import graphlib
import hashlib
import os
from pathlib import Path
from typing import List, Dict, Set, Iterable, TypedDict, Any, Optional
import subprocess
import time
from ui import CargoHackDashboard


#################
### Constants ###
#################

# TODO: Parametrize

# Improvement: These constants are fragile. They rely on WORKSPACE_ROOT pointing to the root of the workspace.
# If this prerequisite is not met, the script will not behave as expected.
# Moving these to parameters would be safer.

CURRENT_FILE_DIRECTORY = Path(__file__).parent.resolve()
WORKSPACE_ROOT = CURRENT_FILE_DIRECTORY.parent.parent
CACHE_DIRECTORY = WORKSPACE_ROOT / ".cache/cargo-hack-check"
GLOBAL_INPUT_FILES = [
    WORKSPACE_ROOT / "Cargo.toml",
    WORKSPACE_ROOT / "rust-toolchain.toml",
    WORKSPACE_ROOT / ".cargo/config.toml",
]
FEATURE_POWERSET_COMMAND = ["cargo", "hack", "check", "--feature-powerset", "--all-targets"]
TAG = "[Cargo Hack Powerset]"
STRICT_WARNING_FLAG = "deny"


###############
### Helpers ###
###############


def ensure_cache_directory_exists():
    CACHE_DIRECTORY.mkdir(parents=True, exist_ok=True)


def normalize_path_to_workspace_root(str_path: str) -> Path:
    path = Path(str_path)
    if not path.is_absolute():
        path = WORKSPACE_ROOT / path
    return path.resolve()


def build_cargo_environment() -> Dict[str, str]:
    env = os.environ.copy()
    rustflags = env.get("CARGO_BUILD_WARNINGS", "").strip()
    if STRICT_WARNING_FLAG not in rustflags:
        env["CARGO_BUILD_WARNINGS"] = f"{rustflags} {STRICT_WARNING_FLAG}".strip()
    return env


def run_in_workspace(command: List[str]) -> str:
    result = subprocess.run(
        command,
        cwd=WORKSPACE_ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=True,
    )
    return result.stdout


def hash_parts(parts: Iterable[str]) -> str:
    hasher = hashlib.sha256()
    for part in parts:
        hasher.update(part.encode())
        hasher.update(b"\0")
    return hasher.hexdigest()


def hash_file(path: Path) -> str:
    if not path.is_file():
        return "MISSING"
    return hashlib.sha256(path.read_bytes()).hexdigest()


###################################################### Workspace #######################################################

#############
### Types ###
#############


@dataclasses.dataclass
class WorkspaceMember:
    id: str
    name: str
    manifest_path: Path
    dependency_ids: Set[str] = dataclasses.field(default_factory=set)

    def __hash__(self):
        return hash(self.id)

    def __eq__(self, other: "WorkspaceMember | Any"):
        if not isinstance(other, WorkspaceMember):
            raise TypeError("Comparison is only supported between WorkspaceMember instances.")
        return self.id == other.id

    @property
    def manifest_path_posix(self) -> str:
        return self.manifest_path.as_posix()

    @property
    def directory(self) -> Path:
        return self.manifest_path.parent

    ### Caching ###

    def get_cache_path(self) -> Path:
        return CACHE_DIRECTORY / f"{self.name}.key"

    def save_cache_key(self, cache_key: str):
        with self.get_cache_path().open("w") as file:
            file.write(cache_key)

    def load_cache_key(self) -> Optional[str]:
        try:
            with self.get_cache_path().open("r") as file:
                return file.read().strip()
        except FileNotFoundError:
            return None

    def is_cache_valid(self, current_cache_key: str) -> bool:
        return self.load_cache_key() == current_cache_key


@dataclasses.dataclass
class Workspace:
    members: List[WorkspaceMember]
    resolve_dispatcher: Dict[str, dict]

    @property
    def member_ids(self) -> Set[str]:
        return {member.id for member in self.members}


class DepsKindEntry(TypedDict, total=False):
    kind: Optional[str]
    target: Optional[str]


class DepsEntry(TypedDict):
    name: str
    pkg: str
    dep_kinds: List[DepsKindEntry]


#############
### Cargo ###
#############


def run_cargo_metadata() -> dict:
    return json.loads(run_in_workspace(["cargo", "metadata", "--format-version", "1"]))


#################
### Workspace ###
#################


def is_dev_only_dependency(dependency_entry: DepsEntry) -> bool:
    """
    Return True if this dependency is *exclusively* a dev-dependency.
    """
    kinds: List[DepsKindEntry] = dependency_entry.get("dep_kinds", [])
    if not kinds:
        return False
    return all(kind.get("kind") == "dev" for kind in kinds)


def filter_non_dev_dependencies(node: dict) -> Iterable[DepsEntry]:
    dependencies: Iterable[DepsEntry] = node.get("deps", [])
    return (
        dependency
        for dependency in dependencies
        if not is_dev_only_dependency(dependency)
    )


def build_workspace(metadata: dict) -> Workspace:
    workspace_member_ids = set(metadata["workspace_members"])
    resolve_dispatcher = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    members = [
        WorkspaceMember(
            id=package["id"],
            name=package["name"],
            manifest_path=normalize_path_to_workspace_root(package["manifest_path"]),
            dependency_ids={
                dependency["pkg"]
                for dependency in filter_non_dev_dependencies(resolve_dispatcher[package["id"]])
                if dependency["pkg"] in workspace_member_ids
            },
        )
        for package in metadata["packages"]
        if package["id"] in workspace_member_ids
    ]
    return Workspace(members=sort_members_topologically(members), resolve_dispatcher=resolve_dispatcher)


def sort_members_topologically(members: List[WorkspaceMember]) -> List[WorkspaceMember]:
    """
    Sort members so dependencies come before their dependents, breaking ties by name.
    Checking dependencies first surfaces a broken crate before its dependents fail on the same error.
    """
    members_dispatcher = {member.id: member for member in members}
    sorter = graphlib.TopologicalSorter({member.id: member.dependency_ids for member in members})
    sorter.prepare()
    sorted_members: List[WorkspaceMember] = []
    while sorter.is_active():
        ready = sorted((members_dispatcher[member_id] for member_id in sorter.get_ready()), key=lambda member: member.name)
        sorted_members.extend(ready)
        sorter.done(*(member.id for member in ready))
    return sorted_members


def get_workspace() -> Workspace:
    return build_workspace(run_cargo_metadata())


##################################################### Cache Keys #######################################################


def compute_global_hash() -> str:
    """
    Hash the inputs that affect every crate's check.
    """
    rustc_version = run_in_workspace(["rustc", "-vV"])
    cargo_hack_version = run_in_workspace(["cargo", "hack", "--version"])
    warnings = build_cargo_environment()["CARGO_BUILD_WARNINGS"]
    return hash_parts([
        *(f"{path.relative_to(WORKSPACE_ROOT)}={hash_file(path)}" for path in GLOBAL_INPUT_FILES),
        rustc_version,
        cargo_hack_version,
        " ".join(FEATURE_POWERSET_COMMAND),
        warnings,
    ])


def compute_source_hash(directory: Path) -> str:
    """
    Hash the tracked and untracked-but-not-ignored files under the directory.
    """
    output = run_in_workspace(["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", directory.as_posix()])
    relative_paths = sorted(set(output.split("\0")) - {""})
    return hash_parts(
        f"{relative_path}={hash_file(WORKSPACE_ROOT / relative_path)}"
        for relative_path in relative_paths
    )


def collect_external_package_ids(member: WorkspaceMember, workspace: Workspace) -> Set[str]:
    """
    Collect the external packages reachable from the member without going through another workspace member.
    Those reached through another workspace member are covered by that member's cache key.
    """
    member_ids = workspace.member_ids
    external_ids: Set[str] = set()
    pending = [member.id]
    while pending:
        node = workspace.resolve_dispatcher[pending.pop()]
        for dependency in filter_non_dev_dependencies(node):
            dependency_id = dependency["pkg"]
            if dependency_id in member_ids or dependency_id in external_ids:
                continue
            external_ids.add(dependency_id)
            pending.append(dependency_id)
    return external_ids


class CacheKeyCalculator:
    def __init__(self, workspace: Workspace):
        self.workspace = workspace
        self.members_dispatcher = {member.id: member for member in workspace.members}
        self.global_hash = compute_global_hash()
        self._cache_keys: Dict[str, str] = {}
        self._in_progress: Set[str] = set()

    def compute(self, member: WorkspaceMember) -> str:
        if member.id in self._cache_keys:
            return self._cache_keys[member.id]
        if member.id in self._in_progress:
            raise RuntimeError(f"Cycle detected in workspace dependencies at {member.name}.")

        self._in_progress.add(member.id)
        dependency_keys = sorted(
            self.compute(self.members_dispatcher[dependency_id])
            for dependency_id in member.dependency_ids
        )
        self._in_progress.remove(member.id)

        cache_key = hash_parts([
            self.global_hash,
            compute_source_hash(member.directory),
            *sorted(collect_external_package_ids(member, self.workspace)),
            *dependency_keys,
        ])
        self._cache_keys[member.id] = cache_key
        return cache_key


##################################################### Cargo Hack #######################################################


class CargoHackCheckCommand:
    def __init__(self, member: WorkspaceMember, cache_key: str):
        self.member = member
        self.cache_key = cache_key

    @property
    def crate_name(self):
        return self.member.name

    @property
    def is_cached(self) -> bool:
        return self.member.is_cache_valid(self.cache_key)

    def as_feature_powerset_command(self) -> List[str]:
        return [*FEATURE_POWERSET_COMMAND, "--manifest-path", self.member.manifest_path_posix]

    def run(self, dashboard) -> int:
        if self.is_cached:
            dashboard.log_crate_detail(self.crate_name, "Cache is valid, skipping.")
            return 0

        dashboard.log_crate_detail(self.crate_name, "Running...")

        result = subprocess.run(
            self.as_feature_powerset_command(),
            capture_output=True,
            text=True,
            check=False,
            env=build_cargo_environment(),
        )

        if result.returncode == 0:
            self.handle_success(dashboard)
        else:
            self.handle_failure(dashboard, result)

        return result.returncode

    # ----------------------------------------------------------

    def handle_success(self, dashboard):
        dashboard.log_crate_detail(self.crate_name, "Succeeded.")
        self.member.save_cache_key(self.cache_key)

    # ----------------------------------------------------------

    def handle_failure(self, dashboard, result):
        dashboard.log_crate_detail(self.crate_name, "Failed.")
        dashboard.log(result.stdout)
        dashboard.log(result.stderr)


def build_cargo_hack_commands() -> List[CargoHackCheckCommand]:
    workspace = get_workspace()
    calculator = CacheKeyCalculator(workspace)
    return [
        CargoHackCheckCommand(member, calculator.compute(member))
        for member in workspace.members
    ]


######################################################## Main ##########################################################


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run cache-aware cargo-hack feature powerset checks for workspace crates.",
    )
    output_mode = parser.add_mutually_exclusive_group()
    output_mode.add_argument(
        "--interactive",
        action="store_true",
        dest="interactive",
        help="Force interactive output even if terminal auto-detection would disable it.",
    )
    output_mode.add_argument(
        "--plain",
        action="store_true",
        dest="plain",
        help="Force plain line-based output even if terminal auto-detection would enable interactive output.",
    )
    parser.add_argument(
        "--continue-on-failure",
        action="store_true",
        help="Continue checking remaining crates after a failure; exit non-zero if any crate fails.",
    )
    return parser.parse_args()


def main(args: argparse.Namespace):
    commands = build_cargo_hack_commands()
    ensure_cache_directory_exists()

    rich_enabled = True if args.interactive else False if args.plain else None
    max_crate_name_width = max((len(command.crate_name) for command in commands), default=0)
    dashboard = CargoHackDashboard(
        len(commands),
        TAG,
        max_crate_name_width=max_crate_name_width,
        rich_enabled=rich_enabled,
    )
    failed_crates: List[str] = []

    try:
        for i, command in enumerate(commands, start=1):

            was_cached = command.is_cached

            dashboard.start_crate(command.crate_name, i)

            crate_started_at = time.monotonic()
            rc = command.run(dashboard)
            crate_elapsed = time.monotonic() - crate_started_at

            dashboard.finish_crate(
                crate_name=command.crate_name,
                index=i,
                skipped=was_cached,
                success=(rc == 0 and not was_cached),
                crate_elapsed=crate_elapsed,
            )

            if rc != 0:
                failed_crates.append(command.crate_name)
                if not args.continue_on_failure:
                    dashboard.fail(command.crate_name)
                    dashboard.print_summary(failed_crates, stopped_early=True)
                    return rc

        dashboard.finish()
        dashboard.print_summary(failed_crates)
        if failed_crates:
            return 1
        return 0

    except KeyboardInterrupt:
        dashboard.interrupt()
        dashboard.close()
        dashboard.print_summary(failed_crates, interrupted=True)
        return 130

    finally:
        dashboard.close()

if __name__ == "__main__":
    status = main(parse_args())
    # TODO: Return different exit code for "everything skipped", to avoid saving cache again.
    exit(status)
