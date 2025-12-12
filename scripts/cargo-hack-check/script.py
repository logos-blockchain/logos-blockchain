#!/usr/bin/env python3
# Runs a cache-aware Cargo Hack check for all crates in the workspace.


import json
import dataclasses
from collections import namedtuple
from pathlib import Path
from sys import stderr
from typing import List, Dict, Set, Iterable, TypedDict, Self, Any
import heapq
import subprocess
import re


#################
### Constants ###
#################

# Improvement: These constants are fragile. They rely on:
# - WORKSPACE_ROOT pointing to the root of the workspace.
# - HASH_SCRIPT being in the same directory as this script.
# If these prerequisites are not met, the script will not behave as expected.
# Moving these to parameters would be safer.

CURRENT_FILE_DIRECTORY = Path(__file__).parent.resolve()
WORKSPACE_ROOT = CURRENT_FILE_DIRECTORY.parent.parent
WORKSPACE_CARGO_LOCK = WORKSPACE_ROOT / "Cargo.lock"
CACHE_DIRECTORY = WORKSPACE_ROOT / ".cache/cargo-hack-check"
COMPUTE_CREATE_HASH_SCRIPT = CURRENT_FILE_DIRECTORY / "compute_crate_hash.sh"
TAG = "[Cargo Hack Powerset]"


###############
### Helpers ###
###############


def with_tag(message: str) -> str:
    return f"{TAG} {message}"


def with_tag_and_crate(crate: str, message: str) -> str:
    return with_tag(f"({crate}) {message}")


def ensure_cache_directory_exists():
    CACHE_DIRECTORY.mkdir(parents=True, exist_ok=True)


def normalize_path_to_workspace_root(str_path: str) -> Path:
    path = Path(str_path)
    if not path.is_absolute():
        path = WORKSPACE_ROOT / path
    return path.resolve()


###################################################### Workspace #######################################################

#############
### Types ###
#############

WorkspaceMemberHeapItem = namedtuple("WorkspaceMemberHeapItem", ["key", "member"])

@dataclasses.dataclass
class WorkspaceMember:
    id: str
    name: str  # Improvement: Ensure these are unique among WorkspaceMember instances
    manifest_path: Path
    dependency_ids: Set[str] = dataclasses.field(default_factory=set)

    def __hash__(self):
        return hash(self.id)

    def __eq__(self, other: WorkspaceMember | Any):
        if not isinstance(other, WorkspaceMember):
            raise TypeError("Comparison is only supported between WorkspaceMember instances.")
        return self.id == other.id

    def get_heap_key(self) -> str:
        return self.name

    def as_heap_item(self) -> WorkspaceMemberHeapItem:
        return WorkspaceMemberHeapItem(self.get_heap_key(), self)

    @property
    def manifest_path_posix(self) -> str:
        return self.manifest_path.as_posix()

    ### Caching ###

    def get_cache_path(self) -> Path:
        return CACHE_DIRECTORY / f"{self.name}.key"

    def compute_cache_key(self) -> str:
        manifest = Path(self.manifest_path)
        return compute_crate_hash(manifest.parent)

    def save_cache_key(self, cache_key: str):
        with self.get_cache_path().open("w") as file:
            file.write(cache_key)

    def load_cache_key(self) -> str | None:
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
    dependencies_dispatcher: Dict[str, Set[WorkspaceMember]]
    dependents_dispatcher: Dict[str, Set[WorkspaceMember]]

    def __post_init__(self):
        assert len(self.members) == len(self.dependencies_dispatcher) == len(self.dependents_dispatcher), "Inconsistent workspace data."


class DepsKindEntry(TypedDict, total=False):
    kind: str | None
    target: str | None


class DepsEntry(TypedDict):
    name: str
    pkg: str
    dep_kinds: List[DepsKindEntry]


#############
### Cargo ###
#############


def run_cargo_metadata() -> dict:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=True,
    )
    return json.loads(result.stdout)


#################
### Workspace ###
#################


def build_workspace_members(workspace_nodes: Iterable[dict], workspace_package_dispatcher: Dict[str, dict]) -> Iterable[WorkspaceMember]:
    return (
        WorkspaceMember(
            id=node["id"],
            name=workspace_package_dispatcher[node["id"]]["name"],
            manifest_path=normalize_path_to_workspace_root(workspace_package_dispatcher[node["id"]]["manifest_path"]),
            dependency_ids={dependency["pkg"] for dependency in filter_workspace_dependencies(node, workspace_package_dispatcher)},
        )
        for node in workspace_nodes
    )

def filter_workspace_nodes(metadata_nodes: List[dict], workspace_member_ids: Set[str]) -> Iterable[dict]:
    return (
        node
        for node in metadata_nodes
        if node["id"] in workspace_member_ids
    )

def is_dev_only_dependency(dependency_entry: DepsEntry) -> bool:
    """
    Return True if this dependency is *exclusively* a dev-dependency.
    """
    kinds: List[DepsKindEntry] = dependency_entry.get("dep_kinds", [])
    if not kinds:
        return False
    return all(kind.get("kind") == "dev" for kind in kinds)


def filter_workspace_dependencies(node: dict, workspace_package_dispatcher: Dict[str, dict]) -> Iterable[DepsEntry]:
    """
    Filter dependencies to include only those that are workspace members and not dev-only dependencies.
    """
    workspace_member_ids = workspace_package_dispatcher.keys()
    dependencies: Iterable[DepsEntry] = node.get("deps", [])
    return (
        dependency
        for dependency in dependencies
        if dependency["pkg"] in workspace_member_ids and not is_dev_only_dependency(dependency)
    )


def filter_workspace_members(metadata: dict) -> List[WorkspaceMember]:
    workspace_member_ids = set(metadata["workspace_members"])
    metadata_nodes = metadata["resolve"]["nodes"]
    workspace_nodes = filter_workspace_nodes(metadata_nodes, workspace_member_ids)
    workspace_package_dispatcher = {
        package["id"]: package
        for package in metadata["packages"]
        if package["id"] in workspace_member_ids
    }
    return list(build_workspace_members(workspace_nodes, workspace_package_dispatcher))


def build_dependencies_dispatcher(members_dispatcher: Dict[str, WorkspaceMember]) -> Dict[str, Set[WorkspaceMember]]:
    return {
        member.id: { members_dispatcher[dependency_id] for dependency_id in member.dependency_ids }
        for member in members_dispatcher.values()
    }

def build_dependents_dispatcher(members_dispatcher: Dict[str, WorkspaceMember]) -> Dict[str, Set[WorkspaceMember]]:
    dependents_dispatcher = {member_id: set() for member_id in members_dispatcher} # Preseed so there's one entry per member
    for member in members_dispatcher.values():
        for dependency_id in member.dependency_ids:
            dependents_dispatcher[dependency_id].add(member)
    return dependents_dispatcher


def build_workspace(metadata: dict) -> Workspace:
    members: List[WorkspaceMember] = filter_workspace_members(metadata)
    members_dispatcher = {member.id: member for member in members}

    # Improvement: Assert both dispatchers' keys are equal to the workspace member IDs.
    dependencies_dispatcher = build_dependencies_dispatcher(members_dispatcher)
    dependents_dispatcher = build_dependents_dispatcher(members_dispatcher)

    return Workspace(
        members=members,
        dependencies_dispatcher=dependencies_dispatcher,
        dependents_dispatcher=dependents_dispatcher,
    )

def sort_workspace_topologically(workspace: Workspace) -> Workspace:
    """
    :param workspace: Mutable, sorted in place by topological order.
    """

    # Clone the dependencies to avoid mutating the original data
    member_id: str
    member_dependencies: Set[WorkspaceMember]
    remaining_dependencies: Dict[str, Set[WorkspaceMember]] = {
        member_id: set(member_dependencies)
        for member_id, member_dependencies in workspace.dependencies_dispatcher.items()
    }
    def degree(member: WorkspaceMember) -> int:
        return len(remaining_dependencies[member.id])

    zero_degree_member_heap = [
        member.as_heap_item() for member in workspace.members if degree(member) == 0
    ]
    heapq.heapify(zero_degree_member_heap)

    sorted_members: List[WorkspaceMember] = []
    while zero_degree_member_heap:
        item: WorkspaceMemberHeapItem = heapq.heappop(zero_degree_member_heap)
        member: WorkspaceMember = item.member
        member_dependents = workspace.dependents_dispatcher[member.id]
        for dependent in member_dependents:
            remaining_dependencies[dependent.id].remove(member)
            if degree(dependent) == 0:
                heapq.heappush(zero_degree_member_heap, dependent.as_heap_item())

        sorted_members.append(item.member)

    if len(sorted_members) != len(workspace.members):
        print(with_tag("Warning: cycle detected in workspace dependencies"), file=stderr)

    workspace.members = sorted_members
    return workspace


def get_workspace_sorted_topologically() -> Workspace:
    metadata = run_cargo_metadata()
    workspace = build_workspace(metadata)
    return sort_workspace_topologically(workspace)


##################################################### Cargo Hack #######################################################

#############
### Types ###
#############

class CargoHackCheckEntry:
    _MANIFEST_RE = re.compile(r"--manifest-path\s+(\S+)")

    def __init__(self, cmd_line: str, manifest_path: str):
        self.cmd_line = cmd_line
        self.manifest_path: Path = normalize_path_to_workspace_root(manifest_path)

    @property
    def manifest_path_posix(self) -> str:
        return self.manifest_path.as_posix()

    @classmethod
    def from_command_line(cls, cmd_line: str) -> Self:
        manifest_path = cls._MANIFEST_RE.search(cmd_line)
        if manifest_path:
            return cls(cmd_line, manifest_path.group(1))
        raise ValueError(f"No manifest path was found in command: {cmd_line}.")

    def __str__(self):
        return f"CargoHackCommand({self.manifest_path})"

    def __repr__(self):
        return f"CargoHackCommand(cmd_line={self.cmd_line}, manifest_path={self.manifest_path})"

    def __hash__(self):
        return hash(self.manifest_path_posix)

    def as_feature_powerset_command(self) -> List[str]:
        return [
            "cargo", "hack", "check", "--feature-powerset", "--no-dev-deps", "--manifest-path", self.manifest_path_posix
        ]


class CargoHackCheckCommand:
    def __init__(self, entry: CargoHackCheckEntry, member: WorkspaceMember, dependents: Set[WorkspaceMember]):
        self.entry = entry
        self.member = member
        self.dependents: Set[WorkspaceMember] = dependents
        assert self.entry.manifest_path_posix == self.member.manifest_path_posix

    @property
    def crate_name(self) -> str:
        return self.member.name

    def run(self) -> int:
        current_cache_key = self.member.compute_cache_key()
        if self.member.is_cache_valid(current_cache_key):
            print(with_tag_and_crate(self.crate_name, "Cache is valid, skipping."), file=stderr)
            return 0

        print(with_tag_and_crate(self.crate_name, "Running..."), file=stderr)
        result = subprocess.run(self.entry.as_feature_powerset_command(), capture_output=True, text=True, check=False)

        # Only invalidate on success. On failure, dependent crates cannot be trusted.
        # Invalidating them would only cause unnecessary work on the next run.
        if result.returncode == 0:
            self.handle_success(current_cache_key)
        else:
            self.handle_failure(result)

        return result.returncode

    def handle_success(self, current_cache_key: str):
        print(with_tag_and_crate(self.crate_name, "Succeeded."), file=stderr)
        print(with_tag_and_crate(self.crate_name, "Updating cache..."), file=stderr)
        self.member.save_cache_key(current_cache_key)
        print(with_tag_and_crate(self.crate_name, "Invalidating dependents..."), file=stderr)
        self.invalidate_dependents()
        print(with_tag_and_crate(self.crate_name, "Done."), file=stderr)

    def handle_failure(self, result: subprocess.CompletedProcess):
        print(with_tag_and_crate(self.crate_name, "Failed."), file=stderr)
        print(result.stdout, end="", file=stderr)
        print(result.stderr, end="", file=stderr)

    def invalidate_dependents(self):
        if len(self.dependents) == 0:
            print(with_tag_and_crate(self.crate_name, "No dependents to invalidate."), file=stderr)
            return

        for dependent in self.dependents:
            print(with_tag_and_crate(self.crate_name, f"Invalidating dependent {dependent.name}..."), file=stderr)
            dependent.get_cache_path().unlink(missing_ok=True)


#################
### Functions ###
#################


def compute_crate_hash(crate_directory: Path) -> str:
    result = subprocess.run(
        [COMPUTE_CREATE_HASH_SCRIPT, crate_directory, WORKSPACE_CARGO_LOCK],
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout.strip()


def list_cargo_hack_check_commands() -> Set[CargoHackCheckEntry]:
    result = subprocess.run(
        ["cargo", "hack", "check", "--no-dev-deps", "--print-command-list"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=True,
    )
    return {CargoHackCheckEntry.from_command_line(line) for line in result.stdout.splitlines()}


def build_cargo_hack_commands_sorted_topologically() -> List[CargoHackCheckCommand]:
    commands = list_cargo_hack_check_commands()
    command_dispatcher = {command.manifest_path_posix: command for command in commands}
    workspace = get_workspace_sorted_topologically()
    sorted_commands = [
        CargoHackCheckCommand(
            command_dispatcher[member.manifest_path.as_posix()],
            member,
            workspace.dependents_dispatcher[member.id]
        )
        for member in workspace.members
    ]
    assert len(sorted_commands) == len(commands), "Some commands are missing in the sorted list. Either the workspace members or commands are inconsistent."
    return sorted_commands


######################################################## Main ##########################################################


def main():
    sorted_commands = build_cargo_hack_commands_sorted_topologically()
    ensure_cache_directory_exists()
    for command in sorted_commands:
        return_code = command.run()
        if return_code != 0:
            # Save time by exiting early since dependent crates cannot be trusted.
            return return_code
    return 0


if __name__ == "__main__":
    status = main()
    exit(status)
