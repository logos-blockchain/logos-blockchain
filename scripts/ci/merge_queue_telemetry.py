#!/usr/bin/env python3
"""Read-only evidence for candidate-scoped native-stack CI evaluation.

``snapshot`` observes candidate ownership, its own stack, trunk queue isolation,
and bounded ancestry of that stack's expected positions 1..P. ``timing`` observes
Code checks, Cucumber, and E2E runs, every retry attempt, and job runner cost.
Neither mode makes CI decisions or executes candidate code; use trusted master.

Raw objective-critical responses accompany derived values. Ordinary PRs remain
``not_a_stack`` even when unrelated stacks are queued. Partial/unavailable stack
lookups remain unresolved. Repository topology never supplies candidate stack
membership. Current queue-entry observations can measure admission-to-workflow
timing; when the entry has left the queue that timing remains unresolved.
Workflow completion and runner-cost completeness are separate: runner seconds
always represent observed work and are a lower bound when accounting is partial.

This is temporary production evidence collection. After representative samples,
retain only signals and measurements needed by the eventual CI optimization.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


TARGET_WORKFLOWS = (
    "Code checks",
    "Cucumber integration tests",
    "End-to-end integration tests",
)
SAFE_ENVIRONMENT_FIELDS = (
    "GITHUB_EVENT_NAME",
    "GITHUB_EVENT_PATH",
    "GITHUB_REF",
    "GITHUB_REF_NAME",
    "GITHUB_REF_TYPE",
    "GITHUB_SHA",
    "GITHUB_BASE_REF",
    "GITHUB_HEAD_REF",
    "GITHUB_RUN_ID",
    "GITHUB_RUN_NUMBER",
    "GITHUB_RUN_ATTEMPT",
    "GITHUB_WORKFLOW",
    "GITHUB_WORKFLOW_REF",
    "GITHUB_WORKFLOW_SHA",
    "GITHUB_REPOSITORY",
    "GITHUB_ACTOR",
    "GITHUB_SERVER_URL",
    "GITHUB_API_URL",
)
QUEUE_SUFFIX_PATTERN = re.compile(
    r"(?:^|/)gh-readonly-queue/(.+)/pr-(\d+)-[0-9a-fA-F]{5,}$"
)
MAX_API_PAGES = 10
MAX_QUEUE_ENTRIES = 100
HTTP_TIMEOUT_SECONDS = 20
GIT_FETCH_DEPTH = 128


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat()


def save_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def normalize_api_error(
    endpoint: str,
    status: int | None,
    error_type: str,
    raw_response: Any = None,
    *,
    timestamp: str | None = None,
) -> dict[str, Any]:
    """Keep sanitized REST failure evidence without headers, tokens, or secrets."""
    detail = _api_error_detail(raw_response) if raw_response is not None else None
    return {
        "endpoint": endpoint,
        "status": status,
        "error_type": error_type,
        "message": detail,
        "timestamp": timestamp or utc_now(),
    }


def copy_event(source: Path, output_dir: Path) -> dict[str, Any]:
    output_dir.mkdir(parents=True, exist_ok=True)
    destination = output_dir / "event.json"
    shutil.copyfile(source, destination)
    return json.loads(destination.read_text(encoding="utf-8"))


def safe_environment() -> dict[str, str | None]:
    return {key: os.environ.get(key) for key in SAFE_ENVIRONMENT_FIELDS}


def normalize_branch_ref(value: Any) -> str | None:
    """Convert a branch ref to its branch name without reinterpreting other refs.

    Plain branch names and ``refs/heads/<name>`` are accepted. Other full ref
    namespaces, such as tags, remain unresolved because they do not identify a
    merge-queue target branch.
    """
    if not isinstance(value, str) or not value:
        return None
    if value.startswith("refs/heads/"):
        branch = value.removeprefix("refs/heads/")
        return branch or None
    if value.startswith("refs/"):
        return None
    return value


def parse_queue_pr_number(ref: Any) -> int | None:
    if not isinstance(ref, str):
        return None
    match = QUEUE_SUFFIX_PATTERN.search(ref)
    return int(match.group(2)) if match else None


def queue_branch_from_queue_ref(ref: Any) -> str | None:
    """Extract the full trunk component from a queue ref as evidence.

    The component may contain slashes; everything between
    ``gh-readonly-queue/`` and ``/pr-<number>-`` is preserved. The naming
    convention remains one identity signal rather than authoritative PR proof.
    """
    if not isinstance(ref, str):
        return None
    match = QUEUE_SUFFIX_PATTERN.search(ref)
    return match.group(1) if match else None


def _pull_numbers(value: Any) -> list[int]:
    if isinstance(value, list):
        return sorted(
            {
                int(item["number"])
                for item in value
                if isinstance(item, dict) and isinstance(item.get("number"), int)
            }
        )
    return []


def derive_candidate_identity(
    *,
    ref_signals: list[tuple[str, Any]],
    head_associated_pulls: Any,
    pull_requests: dict[int, dict[str, Any] | None],
) -> dict[str, Any]:
    """Resolve ownership only when queue ref, head association, and PR agree.

    Head associations may contain cumulative PRs. A unique association alone
    cannot prove ownership; unrelated associated PRs never provide stack data.
    Retain every identity clue when the available signals disagree or fail.
    """
    signals = []
    ref_numbers: set[int] = set()
    head_numbers = _pull_numbers(head_associated_pulls)
    for source, raw_value in ref_signals:
        number = parse_queue_pr_number(raw_value)
        numbers = [number] if number is not None else []
        ref_numbers.update(numbers)
        signals.append({"source": source, "raw_value": raw_value, "candidate_prs": numbers})
    signals.append({
        "source": "head_sha_commit_associations",
        "candidate_prs": head_numbers,
        "interpretation": "May include cumulative PRs; not ownership by itself",
    })
    candidate = None
    evidence = []
    reason = "No unique, independently corroborated queue-ref candidate"
    if len(ref_numbers) > 1:
        reason = "Queue-ref signals disagree"
    elif len(ref_numbers) == 1:
        number = next(iter(ref_numbers))
        metadata = pull_requests.get(number)
        if not isinstance(metadata, dict) or metadata.get("number") != number:
            reason = "Candidate PR metadata unavailable or inconsistent"
        elif number not in head_numbers:
            reason = "Head-SHA association did not corroborate the queue-ref PR"
        else:
            candidate = number
            reason = "Queue ref, head-SHA association, and PR metadata agree"
            evidence = ["queue ref", "head-SHA associated PR", "matching REST PR number"]
    return {
        "candidate_pr": candidate,
        "candidate_pr_confidence": "corroborated" if candidate is not None else "unresolved",
        "candidate_pr_evidence": evidence,
        "candidate_pr_candidates": sorted(ref_numbers | set(head_numbers)),
        "candidate_pr_resolution": reason,
        "signals": signals,
    }


def graphql_response_status(response: Any) -> tuple[str, list[Any]]:
    """Classify GraphQL evidence without discarding usable partial ``data``.

    ``complete`` means a usable response has no GraphQL errors, ``partial``
    means usable data arrived alongside errors, and ``unavailable`` means no
    usable data structure was returned. The original structured errors remain
    available to callers and are written with the raw response.
    """
    if not isinstance(response, dict):
        return "unavailable", []
    errors = response.get("errors")
    errors_list = errors if isinstance(errors, list) else []
    data = response.get("data")
    if not isinstance(data, dict):
        return "unavailable", errors_list
    return ("partial" if errors_list else "complete"), errors_list


def graphql_errors_affect_field(errors: list[Any], *field_names: str) -> bool:
    """Return whether a GraphQL error path names one of the requested fields."""
    for error in errors:
        path = error.get("path") if isinstance(error, dict) else None
        if isinstance(path, list) and any(name in path for name in field_names):
            return True
    return False


def _graphql_pull_request(
    response: Any,
) -> tuple[dict[str, Any] | None, str, list[Any]]:
    status, errors = graphql_response_status(response)
    if status == "unavailable":
        return None, status, errors
    data = response.get("data")
    repository = data.get("repository")
    if not isinstance(repository, dict):
        return None, "unavailable", errors
    pull_request = repository.get("pullRequest")
    if not isinstance(pull_request, dict):
        return None, "unavailable", errors
    return pull_request, status, errors


def classify_stack(graphql_response: Any) -> dict[str, Any]:
    """Classify only the candidate's own complete native-stack lookup.

    Preserve partial field observations and structured errors, but never turn
    partial/unavailable or inconsistent membership into positive classification.
    REST metadata is retained for ownership and failure diagnostics; it does
    not supply an alternative stack classification model.
    Queue entries belonging to other PRs are deliberately not inputs.
    """
    pr, status, errors = _graphql_pull_request(graphql_response)
    result = _unresolved_stack("Candidate stack lookup unavailable or incomplete")
    result.update(graphql_status=status, graphql_errors=errors)
    if pr is None:
        return result
    stack = pr.get("stack")
    entry = pr.get("stackEntry")
    result["observed_stack"] = stack
    result["observed_stack_entry"] = entry
    if status != "complete":
        return result
    if "stack" not in pr or "stackEntry" not in pr:
        return result
    if stack is None and entry is None:
        result.update(
            stack_status="not_a_stack", is_stack_member=False, is_stack_head=False,
            evidence=["Candidate GraphQL stack and stackEntry explicitly null"],
        )
        return result
    if not isinstance(stack, dict) or not isinstance(entry, dict):
        return result
    position, size = entry.get("position"), stack.get("size")
    entry_stack = entry.get("stack")
    if (
        not stack.get("id") or not isinstance(stack.get("number"), int)
        or not isinstance(position, int) or not isinstance(size, int)
        or not 1 <= position <= size
        or not isinstance(entry_stack, dict)
        or entry_stack.get("id") != stack.get("id")
    ):
        return result
    result.update(
        stack_status="stack_member", stack_id=stack["id"],
        stack_number=stack["number"], stack_position=position, stack_size=size,
        stack_base_ref=stack.get("baseRefName"), is_stack_member=True,
        is_stack_head=position == size,
        evidence=["Complete candidate PullRequest.stack and stackEntry lookup"],
    )
    return result


def _unresolved_stack(reason: str) -> dict[str, Any]:
    return {
        "stack_status": "unresolved", "stack_id": None, "stack_number": None,
        "stack_position": None, "stack_size": None, "stack_base_ref": None,
        "is_stack_member": None, "is_stack_head": None, "evidence": [reason],
    }


def _normalize_commit(value: Any) -> str | None:
    """Return a commit OID from a GraphQL commit object when one is present."""
    if isinstance(value, dict) and isinstance(value.get("oid"), str):
        return value["oid"]
    return None


def normalize_merge_queue_entry(value: Any) -> dict[str, Any]:
    """Keep entry identity/admission and candidate-independent stack topology.

    Queue position is scheduling state, separate from native stack position.
    Raw source observations remain available for reconciliation diagnostics.
    """
    value = value if isinstance(value, dict) else {}
    pr = value.get("pullRequest")
    pr = pr if isinstance(pr, dict) else {}
    stack = pr.get("stack") or {}
    stack_entry = pr.get("stackEntry") or {}
    queue = value.get("mergeQueue") or {}
    return {
        "id": value.get("id"), "enqueued_at": value.get("enqueuedAt"),
        "queue_entry_position": value.get("position"), "state": value.get("state"),
        "base_commit": _normalize_commit(value.get("baseCommit")),
        "head_commit": _normalize_commit(value.get("headCommit")),
        "merge_queue": {"id": queue.get("id")} if isinstance(queue, dict) and queue else None,
        "pull_request_number": pr.get("number") if isinstance(pr, dict) else None,
        "stack_id": stack.get("id") if isinstance(stack, dict) else None,
        "stack_number": stack.get("number") if isinstance(stack, dict) else None,
        "stack_size": stack.get("size") if isinstance(stack, dict) else None,
        "stack_position": stack_entry.get("position") if isinstance(stack_entry, dict) else None,
        "raw": value,
    }


def normalize_merge_queue(
    value: Any, *, graphql_errors: list[Any] | None = None
) -> dict[str, Any] | None:
    """Normalize bounded trunk topology independently of candidate classification."""
    if not isinstance(value, dict):
        return None
    entries = value.get("entries")
    normalized_entries: list[dict[str, Any]] = []
    entries_nodes_available = False
    truncated = None
    total_count = None
    page_info: dict[str, Any] | None = None
    if isinstance(entries, dict):
        total_count = entries.get("totalCount")
        page_info_value = entries.get("pageInfo")
        if isinstance(page_info_value, dict):
            page_info = {
                "has_next_page": page_info_value.get("hasNextPage"),
                "end_cursor": page_info_value.get("endCursor"),
            }
        nodes = entries.get("nodes")
        if isinstance(nodes, list):
            entries_nodes_available = True
            normalized_entries = [normalize_merge_queue_entry(node) for node in nodes]
            for entry in normalized_entries:
                if entry.get("merge_queue") is None:
                    entry["merge_queue"] = {"id": value.get("id")}
        truncated = bool(
            page_info and page_info.get("has_next_page")
        ) or (
            isinstance(total_count, int) and total_count > len(normalized_entries)
        )
    return {
        "id": value.get("id"),
        "entries": normalized_entries,
        "total_count": total_count,
        "page_info": page_info,
        "truncated": truncated,
        "entries_available": isinstance(entries, dict)
        and entries_nodes_available,
        "entries_complete": (
            isinstance(entries, dict)
            and entries_nodes_available
            and isinstance(page_info, dict)
            and page_info.get("has_next_page") is False
            and truncated is False
            and all(isinstance(entry.get("pull_request_number"), int)
                    for entry in normalized_entries)
            and not graphql_errors_affect_field(
                graphql_errors or [], "entries", "nodes", "pageInfo", "totalCount",
                "pullRequest", "number"
            )
            and not any(
                isinstance(error, dict)
                and isinstance(error.get("path"), list)
                and error["path"][-1:] == ["mergeQueue"]
                for error in graphql_errors or []
            )
        ),
        "raw": value,
    }


def normalize_merge_queue_response(response: Any) -> dict[str, Any]:
    """Separate current entry state from repository topology availability.

    ``absent`` means the API answered and returned a null entry; ``unresolved``
    means the response could not establish the current state. Queue topology is
    normalized from the repository-level ``mergeQueue`` when present, while
    ``isInMergeQueue`` remains an independent signal.
    GraphQL ``partial`` responses retain usable entry/topology fields and their
    structured errors; ``unavailable`` responses leave them unresolved.
    """
    graphql_status, graphql_errors = graphql_response_status(response)
    if graphql_status == "unavailable":
        return {
            "queue_entry_status": "unresolved",
            "entry": None,
            "merge_queue": None,
            "is_in_merge_queue": None,
            "graphql_available": False,
            "graphql_status": graphql_status,
            "graphql_errors": graphql_errors,
        }
    data = response.get("data")
    repository = data.get("repository") if isinstance(data, dict) else None
    if not isinstance(repository, dict):
        return {
            "queue_entry_status": "unresolved",
            "entry": None,
            "merge_queue": None,
            "is_in_merge_queue": None,
            "graphql_available": False,
            "graphql_status": "unavailable",
            "graphql_errors": graphql_errors,
        }
    pull_request = repository.get("pullRequest")
    entry_field_present = (
        isinstance(pull_request, dict) and "mergeQueueEntry" in pull_request
    )
    entry_field_error = graphql_errors_affect_field(
        graphql_errors, "mergeQueueEntry"
    )
    entry_value = pull_request.get("mergeQueueEntry") if isinstance(pull_request, dict) else None
    queue_value = repository.get("mergeQueue")
    graphql_available = "pullRequest" in repository or "mergeQueue" in repository
    return {
        "queue_entry_status": (
            "present" if isinstance(entry_value, dict)
            else "absent"
            if isinstance(pull_request, dict) and entry_field_present and entry_value is None
            and not entry_field_error
            else "unresolved"
        ),
        "entry": normalize_merge_queue_entry(entry_value)
        if isinstance(entry_value, dict)
        else None,
        "merge_queue": normalize_merge_queue(
            queue_value, graphql_errors=graphql_errors
        ),
        "is_in_merge_queue": (
            pull_request.get("isInMergeQueue")
            if isinstance(pull_request, dict)
            else None
        ),
        "graphql_available": graphql_available,
        "graphql_status": graphql_status,
        "graphql_errors": graphql_errors,
    }


def reconcile_candidate_queue_entry(
    candidate_queue: dict[str, Any],
    repository_queue: dict[str, Any],
    candidate_pr: int | None,
    *,
    candidate_observed_at: str | None = None,
    repository_observed_at: str | None = None,
) -> dict[str, Any]:
    """Reconcile PR-specific and repository-trunk queue-entry evidence.

    Repository entries are matched only by an already resolved PR number. A
    unique match can fill a missing PR-specific entry; duplicate matches or
    material disagreement leave the effective entry unresolved while retaining
    both source records and conflict details. Queue position, state, and
    admission timestamps are mutable observation drift, not identity. When
    both views identify the same entry, repository queue values are preferred
    as the later trunk-level state. Absence is proven only by a complete
    repository snapshot with no matching PR entry; an unavailable/truncated
    second view leaves a PR-level null unresolved because a stack member may
    participate in a different trunk queue. If the later complete snapshot
    omits a candidate that had an earlier PR-specific entry, preserve both
    observations but mark the effective entry unresolved as a stale-observation
    conflict rather than using the older admission timestamp.
    """
    pr_entry = candidate_queue.get("entry") if isinstance(candidate_queue, dict) else None
    queue_snapshot = repository_queue.get("merge_queue") or {}
    entries = queue_snapshot.get("entries", [])
    matches = [
        entry for entry in entries
        if isinstance(entry, dict) and candidate_pr is not None
        and entry.get("pull_request_number") == candidate_pr
    ]
    repository_entry = matches[0] if len(matches) == 1 else None
    identity_fields = ("id", "head_commit", "base_commit")
    identity_mismatches: list[str] = []
    drift_fields: list[str] = []
    if isinstance(pr_entry, dict) and isinstance(repository_entry, dict):
        for field in identity_fields:
            left = pr_entry.get(field)
            right = repository_entry.get(field)
            if left is not None and right is not None and left != right:
                identity_mismatches.append(field)
        for field in (
            "queue_entry_position",
            "state",
            "enqueued_at",
        ):
            left = pr_entry.get(field)
            right = repository_entry.get(field)
            if left is not None and right is not None and left != right:
                drift_fields.append(field)
        left_queue = (pr_entry.get("merge_queue") or {}).get("id")
        right_queue = (repository_entry.get("merge_queue") or {}).get("id")
        if left_queue and right_queue and left_queue != right_queue:
            identity_mismatches.append("merge_queue.id")
    ambiguous = len(matches) > 1
    identity_conflict = bool(identity_mismatches) or ambiguous
    repository_complete = bool(queue_snapshot.get("entries_complete"))
    stale_candidate_entry = (
        isinstance(pr_entry, dict)
        and candidate_pr is not None
        and len(matches) == 0
        and repository_complete
    )
    if identity_conflict or stale_candidate_entry:
        effective = None
        source = "unresolved"
    elif isinstance(pr_entry, dict) and isinstance(repository_entry, dict):
        effective = repository_entry
        source = "corroborated"
    elif isinstance(pr_entry, dict):
        effective = pr_entry
        source = "pull_request.mergeQueueEntry"
    elif isinstance(repository_entry, dict):
        effective = repository_entry
        source = "repository.mergeQueue.entries"
    else:
        effective = None
        source = "unresolved"
    candidate_status = (
        candidate_queue.get("queue_entry_status", "unresolved")
        if isinstance(candidate_queue, dict)
        else "unresolved"
    )
    if isinstance(effective, dict):
        entry_status = "present"
    elif identity_conflict or stale_candidate_entry:
        entry_status = "unresolved"
    elif (
        candidate_pr is not None
        and len(matches) == 0
        and repository_complete
        and candidate_status in {"absent", "unresolved"}
    ):
        # A complete trunk snapshot is sufficient even if the candidate query
        # failed; it positively enumerates the queue without this PR.
        entry_status = "absent"
    else:
        entry_status = "unresolved"
    return {
        "candidate_entry_from_pull_request": pr_entry,
        "candidate_entry_from_repository_queue": repository_entry,
        "repository_matching_entry_count": len(matches),
        "repository_matching_entries": matches,
        "candidate_entry_from_pull_request_observed_at": candidate_observed_at,
        "candidate_entry_from_repository_queue_observed_at": repository_observed_at,
        "effective_candidate_queue_entry": effective,
        "effective_candidate_queue_entry_source": source,
        "candidate_entry_identity_conflict": identity_conflict,
        "candidate_entry_identity_conflict_fields": identity_mismatches,
        "candidate_entry_stale_observation_conflict": stale_candidate_entry,
        "candidate_entry_observation_drift_fields": drift_fields,
        "queue_entry_status": entry_status,
        "repository_candidate_entry_resolution": (
            "ambiguous" if ambiguous
            else "matched" if len(matches) == 1
            else "not_found_complete_snapshot"
            if candidate_pr is not None and repository_complete
            else "not_found_incomplete_snapshot"
            if candidate_pr is not None
            else "candidate_unresolved"
        ),
    }


def resolve_queue_branch(
    *, merge_group_base_ref: Any = None,
    stack: dict[str, Any] | None = None,
    pull_request: dict[str, Any] | None = None,
) -> dict[str, str | None]:
    """Use raw merge-group base, own-stack trunk, or proven ordinary PR base.

    Normalize only refs/heads/ when a branch name is needed. An intermediate
    stack member's direct base never supplies the trunk; missing evidence stays
    unresolved. The event retains the original base_ref verbatim.
    """
    branch = normalize_branch_ref(merge_group_base_ref)
    if branch:
        return {"queue_branch": branch, "queue_branch_source": "merge_group.base_ref"}
    stack = stack or {}
    if stack.get("stack_status") == "stack_member":
        branch = normalize_branch_ref(stack.get("stack_base_ref"))
        if branch:
            return {"queue_branch": branch, "queue_branch_source": "GraphQL stack base"}
    if stack.get("stack_status") == "not_a_stack":
        base = (pull_request or {}).get("base") or {}
        branch = normalize_branch_ref(base.get("ref") if isinstance(base, dict) else None)
        if branch:
            return {"queue_branch": branch, "queue_branch_source": "REST PR base"}
    return {"queue_branch": None, "queue_branch_source": "unresolved"}


def derive_queue_timing(
    collected_at: Any, queue_entry: dict[str, Any] | None,
    workflow_runs: list[dict[str, Any]], *, candidate_sha: str | None = None,
) -> dict[str, Any]:
    """Measure current entry admission to this SHA's target Actions milestones.

    No historical admission is reconstructed. Negative durations or missing
    entry evidence stay null; current requeues cannot be paired with earlier
    dispatches. Completion means latest observed Code/Cucumber/E2E attempts,
    never required-check or final-merge completion.
    """
    enqueued = (queue_entry or {}).get("enqueued_at")
    runs = [run for run in workflow_runs if run.get("name") in TARGET_WORKFLOWS
            and run.get("event") == "merge_group" and candidate_sha
            and run.get("head_sha") == candidate_sha]
    created = [run.get("created_at") for run in runs if run.get("created_at")]
    started = [run.get("run_started_at") for run in runs if run.get("run_started_at")]
    latest = _latest_runs_by_name(runs)
    complete = all(latest.get(name, {}).get("status") == "completed" for name in TARGET_WORKFLOWS)
    completed = [run.get("updated_at") for run in latest.values() if run.get("updated_at")]
    # A later admission is not evidence for earlier checks, including reruns.
    same_cycle = bool(enqueued and created and duration_seconds(enqueued, min(created)) is not None)
    return {
        "queue_age_at_snapshot_seconds": duration_seconds(enqueued, collected_at),
        "queue_to_first_target_workflow_created_seconds": (
            duration_seconds(enqueued, min(created)) if same_cycle else None
        ),
        "queue_to_first_target_workflow_started_seconds": (
            duration_seconds(enqueued, min(started)) if same_cycle and started else None
        ),
        "queue_to_target_workflows_complete_seconds": (
            duration_seconds(enqueued, max(completed))
            if same_cycle and complete and completed else None
        ),
    }


def _parse_datetime(value: Any) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def duration_seconds(start: Any, end: Any) -> float | None:
    start_time = _parse_datetime(start)
    end_time = _parse_datetime(end)
    if start_time is None or end_time is None:
        return None
    seconds = (end_time - start_time).total_seconds()
    return round(seconds, 3) if seconds >= 0 else None


def classify_runner(job: dict[str, Any]) -> str:
    """Classify a job as self-hosted, GitHub-hosted, or unknown from API metadata."""
    labels = [str(label).casefold() for label in job.get("labels", []) or []]
    runner_name = str(job.get("runner_name") or "").casefold()
    group_name = str(job.get("runner_group_name") or "").casefold()
    if "self-hosted" in labels:
        return "self-hosted"
    if "github actions" in group_name or runner_name.startswith("github actions"):
        return "github-hosted"
    return "unknown"


def runner_metadata_missing_unexpectedly(job: dict[str, Any]) -> bool:
    """Report missing runner metadata only when a job appears to have run."""
    conclusion = job.get("conclusion")
    if conclusion == "skipped":
        return False
    completed_after_start = (
        job.get("status") == "completed"
        and conclusion not in {None, "cancelled", "startup_failure"}
    )
    if not job.get("started_at") and not completed_after_start:
        return False
    return not (
        job.get("runner_name")
        or job.get("runner_group_name")
        or job.get("labels")
    )


def _job_runner_cost_issue(job: dict[str, Any]) -> str | None:
    """Return why a job makes runner-cost coverage partial, if it does.

    Skips cannot have runner work to account for. A job known not to have run
    is exempt only when timestamps provide no evidence of measurable work.
    Other jobs must be terminal with valid non-negative runtime timestamps,
    including cancelled jobs.
    """
    conclusion = job.get("conclusion")
    started_at = job.get("started_at")
    if conclusion == "skipped":
        return None
    if conclusion in {"startup_failure", "action_required"} and not started_at:
        return None
    if conclusion == "cancelled" and not started_at:
        return None
    if job.get("status") != "completed":
        return "job_incomplete"
    if duration_seconds(started_at, job.get("completed_at")) is None:
        return "job_runtime_unavailable"
    return None


def derive_timing_metrics(
    latest_runs: dict[str, dict[str, Any] | None],
    jobs_by_run: dict[tuple[int, int], list[dict[str, Any]]],
    all_runs: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    """Normalize per-attempt timings and sum runner work across retries.

    Each workflow attempt has its own (run ID, attempt) key. Runner seconds sum
    observed completed work from every attempt, including cancelled attempts
    that ran jobs. ``timing_status`` describes latest workflow completion;
    ``runner_time_status`` separately reports whether every attempt's job
    listing and runner-work timestamps are complete. Running jobs and jobs
    with unmeasurable runtimes make accounting partial; skipped or never-started
    jobs do not. Observed seconds remain available as a lower bound.
    Candidate wall time uses earliest creation to latest completion and does
    not sum retry durations.
    """
    workflow_rows: list[dict[str, Any]] = []
    all_latest = [run for run in latest_runs.values() if isinstance(run, dict)]
    for name in TARGET_WORKFLOWS:
        run = latest_runs.get(name)
        if not isinstance(run, dict):
            workflow_rows.append(
                {
                    "workflow_name": name,
                    "run_id": None,
                    "status": "not_observed",
                    "conclusion": None,
                    "workflow_created_to_run_started_seconds": None,
                    "runtime_seconds": None,
                }
            )
            continue
        run_id = run.get("id")
        attempt = run.get("run_attempt") or 1
        is_complete = run.get("status") == "completed"
        end = run.get("updated_at") if is_complete else None
        workflow_rows.append(
            {
                "workflow_name": name,
                "run_id": run_id,
                "run_attempt": attempt,
                "event": run.get("event"),
                "head_branch": run.get("head_branch"),
                "head_sha": run.get("head_sha"),
                "status": run.get("status"),
                "conclusion": run.get("conclusion"),
                "created_at": run.get("created_at"),
                "run_started_at": run.get("run_started_at"),
                "updated_at": run.get("updated_at"),
                "html_url": run.get("html_url"),
                "workflow_created_to_run_started_seconds": duration_seconds(
                    run.get("created_at"), run.get("run_started_at")
                ),
                "runtime_seconds": duration_seconds(run.get("run_started_at"), end),
            }
        )

    complete = all(
        isinstance(latest_runs.get(name), dict)
        and latest_runs[name].get("status") == "completed"
        for name in TARGET_WORKFLOWS
    )
    observed_target_runs = [
        run
        for run in (all_runs if all_runs is not None else all_latest)
        if (run.get("name") or run.get("workflow_name")) in TARGET_WORKFLOWS
    ]
    runner_time_missing_attempts = []
    for run in observed_target_runs:
        if (
            run.get("status") != "completed"
            and run.get("attempt_metadata_available") is not False
        ):
            runner_time_missing_attempts.append({
                "run_id": run.get("id"),
                "run_attempt": int(run.get("run_attempt") or 1),
                "workflow_name": run.get("name") or run.get("workflow_name"),
                "reason": "workflow_incomplete",
                "status": run.get("status"),
            })
    for (run_id, attempt), jobs in jobs_by_run.items():
        run = next(
            (item for item in observed_target_runs
             if item.get("id") == run_id
             and int(item.get("run_attempt") or 1) == attempt),
            {},
        )
        for job in jobs:
            reason = _job_runner_cost_issue(job)
            if reason is not None:
                runner_time_missing_attempts.append({
                    "run_id": run_id,
                    "run_attempt": attempt,
                    "workflow_name": (
                        job.get("workflow_name") or run.get("name")
                        or run.get("workflow_name")
                    ),
                    "reason": reason,
                    "job_id": job.get("id"),
                    "job_name": job.get("name"),
                })
    for run in observed_target_runs:
        run_id = run.get("id")
        attempt = int(run.get("run_attempt") or 1)
        key = (run_id, attempt)
        jobs_complete = run.get("jobs_complete")
        if jobs_complete is None:
            jobs_complete = key in jobs_by_run
        if jobs_complete is not True or key not in jobs_by_run:
            runner_time_missing_attempts.append(
                {
                    "run_id": run_id,
                    "run_attempt": attempt,
                    "workflow_name": run.get("name") or run.get("workflow_name"),
                    "reason": "jobs_unavailable",
                    "attempt_metadata_available": run.get(
                        "attempt_metadata_available"
                    ),
                }
            )
    created_times = [
        timestamp
        for run in observed_target_runs
        if (timestamp := _parse_datetime(run.get("created_at"))) is not None
    ]
    completed_times = [
        timestamp
        for run in observed_target_runs
        if run.get("status") == "completed"
        and (timestamp := _parse_datetime(run.get("updated_at"))) is not None
    ]
    earliest = min(created_times, default=None)
    latest_completion = max(completed_times, default=None)
    candidate_wall = (
        round((latest_completion - earliest).total_seconds(), 3)
        if complete and earliest is not None and latest_completion is not None
        else None
    )

    runner_seconds = {"self-hosted": 0.0, "github-hosted": 0.0, "unknown": 0.0}
    runner_workflow_seconds: dict[str, float] = {}
    job_rows: list[dict[str, Any]] = []
    run_by_key = {
        (run.get("id"), int(run.get("run_attempt") or 1)): run
        for run in (all_runs if all_runs is not None else all_latest)
        if isinstance(run, dict) and isinstance(run.get("id"), int)
    }
    for (run_id, attempt), jobs in sorted(jobs_by_run.items()):
        for job in jobs:
            runtime = duration_seconds(job.get("started_at"), job.get("completed_at"))
            runner_class = classify_runner(job)
            run = run_by_key.get((run_id, attempt), {})
            if (
                runtime is not None
                and job.get("status") == "completed"
                and job.get("conclusion") != "skipped"
            ):
                runner_seconds[runner_class] += runtime
                workflow_name = str(job.get("workflow_name") or run.get("name") or "unknown")
                runner_workflow_seconds[workflow_name] = (
                    runner_workflow_seconds.get(workflow_name, 0.0) + runtime
                )
            job_row = {
                "workflow_name": job.get("workflow_name") or run.get("name"),
                "run_id": run_id,
                "run_attempt": attempt,
                "job_id": job.get("id"),
                "job_name": job.get("name"),
                "status": job.get("status"),
                "conclusion": job.get("conclusion"),
                "started_at": job.get("started_at"),
                "completed_at": job.get("completed_at"),
                "runner_name": job.get("runner_name"),
                "runner_classification": runner_class,
                "runtime_seconds": runtime,
                "workflow_created_to_job_started_seconds": duration_seconds(
                    run.get("created_at"), job.get("started_at")
                ),
                "workflow_run_started_to_job_started_seconds": duration_seconds(
                    run.get("run_started_at"), job.get("started_at")
                ),
            }
            job_rows.append(job_row)

    for runner_class in runner_seconds:
        runner_seconds[runner_class] = round(runner_seconds[runner_class], 3)
    runner_workflow_seconds = {
        workflow: round(seconds, 3)
        for workflow, seconds in runner_workflow_seconds.items()
    }

    return {
        "timing_status": "complete" if complete else "partial",
        "runner_time_status": (
            "partial" if runner_time_missing_attempts else "complete"
        ),
        "runner_time_missing_attempts": runner_time_missing_attempts,
        "target_workflows": list(TARGET_WORKFLOWS),
        "workflow_rows": workflow_rows,
        "candidate_wall_time_seconds": candidate_wall,
        "runner_time_seconds": runner_seconds,
        "runner_time_seconds_by_workflow": runner_workflow_seconds,
        "job_rows": job_rows,
    }


class GitHubAPI:
    def __init__(self, warnings: list[dict[str, str]]) -> None:
        self.warnings = warnings
        self.api_errors: list[dict[str, Any]] = []
        self.token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
        self.api_base = os.environ.get("GITHUB_API_URL", "https://api.github.com").rstrip("/")
        self.repo_name = os.environ.get("GITHUB_REPOSITORY", "")
        self.owner, separator, self.repo = self.repo_name.partition("/")
        if not separator:
            self.owner = ""
            self.repo = ""
        if self.api_base.endswith("/api/v3"):
            self.graphql_url = self.api_base[: -len("/api/v3")] + "/api/graphql"
        else:
            self.graphql_url = self.api_base + "/graphql"

    def _request(
        self,
        url: str,
        *,
        method: str = "GET",
        payload: dict[str, Any] | None = None,
        warning_label: str,
    ) -> Any:
        if not self.token:
            self.api_errors.append(
                normalize_api_error(warning_label, None, "missing_token")
            )
            self.warnings.append(
                {
                    "category": "api_unavailable",
                    "message": "GitHub token is unavailable",
                    "endpoint": warning_label,
                }
            )
            return None
        body = json.dumps(payload).encode("utf-8") if payload is not None else None
        headers = {
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {self.token}",
            "User-Agent": "logos-merge-queue-telemetry",
        }
        if not url.endswith("/graphql"):
            headers["X-GitHub-Api-Version"] = "2026-03-10"
        if body is not None:
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(url, data=body, headers=headers, method=method)
        try:
            with urllib.request.urlopen(request, timeout=HTTP_TIMEOUT_SECONDS) as response:
                raw = response.read()
        except urllib.error.HTTPError as error:
            raw = error.read()
            detail = _api_error_detail(raw)
            self.api_errors.append(
                normalize_api_error(
                    warning_label, error.code, "http_error", raw
                )
            )
            self.warnings.append(
                {
                    "category": "api_http_error",
                    "endpoint": warning_label,
                    "message": f"GitHub API returned HTTP {error.code}: {detail}",
                }
            )
            return None
        except (urllib.error.URLError, TimeoutError) as error:
            self.api_errors.append(
                normalize_api_error(
                    warning_label, None, type(error).__name__, str(error)
                )
            )
            self.warnings.append(
                {
                    "category": "api_request_error",
                    "endpoint": warning_label,
                    "message": f"GitHub API request failed: {type(error).__name__}",
                }
            )
            return None
        try:
            return json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            self.api_errors.append(
                normalize_api_error(
                    warning_label, None, "invalid_json", raw
                )
            )
            self.warnings.append(
                {
                    "category": "api_invalid_response",
                    "endpoint": warning_label,
                    "message": "GitHub API response was not valid JSON",
                }
            )
            return None

    def rest(
        self,
        path: str,
        *,
        params: dict[str, Any] | None = None,
        warning_label: str | None = None,
    ) -> Any:
        url = self.api_base + "/" + path.lstrip("/")
        if params:
            url += "?" + urllib.parse.urlencode(params)
        return self._request(
            url, warning_label=warning_label or path
        )

    def graphql(self, query: str, variables: dict[str, Any], warning_label: str) -> Any:
        return self._request(
            self.graphql_url,
            method="POST",
            payload={"query": query, "variables": variables},
            warning_label=warning_label,
        )

    def associated_pulls(self, sha: str) -> Any:
        if not self.owner or not self.repo:
            self.warnings.append(
                {"category": "api_configuration", "message": "GITHUB_REPOSITORY is unavailable"}
            )
            return None
        owner = urllib.parse.quote(self.owner, safe="")
        repo = urllib.parse.quote(self.repo, safe="")
        return self.rest(
            f"repos/{owner}/{repo}/commits/{urllib.parse.quote(sha, safe='')}/pulls",
            params={"per_page": MAX_QUEUE_ENTRIES},
            warning_label=f"associated pull requests for commit {sha}",
        )


    def pull_request(self, number: int) -> Any:
        if not self.owner or not self.repo:
            return None
        owner = urllib.parse.quote(self.owner, safe="")
        repo = urllib.parse.quote(self.repo, safe="")
        return self.rest(
            f"repos/{owner}/{repo}/pulls/{number}",
            warning_label=f"pull request #{number}",
        )

    def workflow_run_attempt(self, run_id: int, attempt: int) -> Any:
        if not self.owner or not self.repo:
            return None
        owner = urllib.parse.quote(self.owner, safe="")
        repo = urllib.parse.quote(self.repo, safe="")
        return self.rest(
            f"repos/{owner}/{repo}/actions/runs/{run_id}/attempts/{attempt}",
            warning_label=f"workflow run {run_id}, attempt {attempt}",
        )


    def stack_graphql(self, number: int) -> Any:
        """Read the candidate's own native stack and bounded member identities."""
        query = """
        query($owner: String!, $name: String!, $number: Int!) {
          repository(owner: $owner, name: $name) {
            pullRequest(number: $number) {
              number
              stack {
                id number size baseRefName
                entries(first: __MAX_QUEUE_ENTRIES__) {
                  totalCount
                  nodes {
                    position
                    pullRequest { number baseRefOid headRefOid }
                  }
                }
              }
              stackEntry { position stack { id } }
            }
          }
        }
        """.replace("__MAX_QUEUE_ENTRIES__", str(MAX_QUEUE_ENTRIES))
        return self.graphql(
            query, {"owner": self.owner, "name": self.repo, "number": number},
            f"GraphQL candidate stack for PR #{number}",
        )

    def candidate_queue_entry_graphql(self, number: int) -> Any:
        """Read candidate entry independently of repository queue topology."""
        query = """
        query($owner: String!, $name: String!, $number: Int!) {
          repository(owner: $owner, name: $name) {
            pullRequest(number: $number) {
              number isInMergeQueue
              mergeQueueEntry {
                id enqueuedAt position state
                baseCommit { oid } headCommit { oid }
                pullRequest { number }
                mergeQueue { id }
              }
            }
          }
        }
        """
        return self.graphql(
            query, {"owner": self.owner, "name": self.repo, "number": number},
            f"GraphQL candidate queue entry for PR #{number}",
        )

    def repository_merge_queue_graphql(self, queue_branch: str) -> Any:
        """Read bounded trunk topology for isolation, reconciliation, and admission."""
        query = """
        query($owner: String!, $name: String!, $branch: String!) {
          repository(owner: $owner, name: $name) {
            mergeQueue(branch: $branch) {
              id
              entries(first: __MAX_QUEUE_ENTRIES__) {
                totalCount pageInfo { hasNextPage endCursor }
                nodes {
                  id enqueuedAt position state
                  baseCommit { oid } headCommit { oid }
                  pullRequest {
                    number
                    stack { id number size }
                    stackEntry { position }
                  }
                }
              }
            }
          }
        }
        """.replace("__MAX_QUEUE_ENTRIES__", str(MAX_QUEUE_ENTRIES))
        return self.graphql(
            query, {"owner": self.owner, "name": self.repo, "branch": queue_branch},
            f"GraphQL repository queue for {queue_branch}",
        )


def _api_error_detail(raw: Any) -> str:
    if isinstance(raw, str):
        return raw[:240]
    if isinstance(raw, dict):
        message = raw.get("message")
        return message[:240] if isinstance(message, str) else "response body omitted"
    if not isinstance(raw, (bytes, bytearray)):
        return "response body omitted"
    try:
        value = json.loads(bytes(raw).decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return "response body omitted"
    if isinstance(value, dict) and isinstance(value.get("message"), str):
        return value["message"][:240]
    return "response body omitted"


def _write_raw_api(path: Path, value: Any) -> None:
    if value is not None:
        save_json(path, value)


def _write_api_errors(api: GitHubAPI, output_dir: Path) -> None:
    """Persist sanitized REST failure records without credentials or headers."""
    save_json(output_dir / "api" / "api-errors.json", api.api_errors)


def _git(
    repo: Path,
    args: list[str],
    warnings: list[dict[str, str]],
    *,
    label: str,
) -> str | None:
    completed = subprocess.run(
        ["git", "-C", str(repo), *args],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if completed.returncode != 0:
        warnings.append(
            {
                "category": "git_unavailable",
                "message": f"{label} failed (exit {completed.returncode})",
            }
        )
        return None
    return completed.stdout


def bounded_git_fetch_args(ref: str) -> list[str]:
    """Build an explicit, bounded fetch command for a ref or object SHA."""
    return ["fetch", "--no-tags", f"--depth={GIT_FETCH_DEPTH}", "origin", ref]


def ancestry_from_exit_code(
    return_code: int, history_shallow: bool | None
) -> bool | None:
    """Interpret ancestor checks conservatively when bounded history is shallow."""
    if return_code == 0:
        return True
    if return_code == 1:
        return False if history_shallow is False else None
    return None


def _git_is_ancestor(
    repo: Path,
    possible_ancestor: Any,
    possible_descendant: Any,
    warnings: list[dict[str, str]],
    *,
    history_shallow: bool | None = None,
) -> bool | None:
    if (
        not isinstance(possible_ancestor, str)
        or not re.fullmatch(r"[0-9a-fA-F]{40,64}", possible_ancestor)
        or not isinstance(possible_descendant, str)
        or not re.fullmatch(r"[0-9a-fA-F]{40,64}", possible_descendant)
    ):
        return None
    completed = subprocess.run(
        [
            "git",
            "-C",
            str(repo),
            "merge-base",
            "--is-ancestor",
            possible_ancestor,
            possible_descendant,
        ],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if completed.returncode == 0:
        return True
    if completed.returncode == 1:
        result = ancestry_from_exit_code(
            completed.returncode, history_shallow
        )
        if result is None and not any(
            warning.get("category") == "git_ancestry_incomplete"
            for warning in warnings
        ):
            warnings.append(
                {
                    "category": "git_ancestry_incomplete",
                    "message": (
                        "A negative ancestry result is unresolved because bounded Git "
                        "history is shallow or its completeness could not be determined"
                    ),
                }
            )
        return result
    warnings.append(
        {
            "category": "git_ancestry_unavailable",
            "message": "Could not compare a PR head SHA with the candidate ancestry",
        }
    )
    return None


def _api_run_pages(
    api: GitHubAPI, head_sha: str, output_dir: Path, warnings: list[dict[str, str]]
) -> tuple[list[dict[str, Any]], list[Any]]:
    """Collect bounded Actions workflow-run pages for a candidate SHA."""
    all_runs: list[dict[str, Any]] = []
    raw_pages: list[Any] = []
    for page in range(1, MAX_API_PAGES + 1):
        data = api.rest(
            "repos/" + urllib.parse.quote(api.repo_name, safe="/") + "/actions/runs",
            params={"head_sha": head_sha, "per_page": 100, "page": page},
            warning_label=f"Actions runs for head SHA {head_sha}, page {page}",
        )
        if data is None:
            break
        raw_pages.append(data)
        _write_raw_api(output_dir / f"page-{page}.json", data)
        runs = data.get("workflow_runs", []) if isinstance(data, dict) else []
        if isinstance(runs, list):
            all_runs.extend(item for item in runs if isinstance(item, dict))
        if not isinstance(runs, list) or len(runs) < 100:
            break
        if page == MAX_API_PAGES:
            warnings.append(
                {
                    "category": "api_page_limit",
                    "message": f"Actions run listing reached the {MAX_API_PAGES}-page limit",
                }
            )
    return all_runs, raw_pages


def _api_job_pages(
    api: GitHubAPI,
    run_id: int,
    attempt: int,
    output_dir: Path,
    warnings: list[dict[str, str]],
) -> tuple[list[dict[str, Any]], list[Any], bool]:
    """Collect bounded job pages and report whether the listing is complete.

    A successful empty page is complete evidence of zero observed jobs. An API
    failure, malformed page, or exhausted page bound returns ``False`` so
    runner totals can remain visible while their completeness is marked partial.
    """
    jobs: list[dict[str, Any]] = []
    raw_pages: list[Any] = []
    complete = False
    for page in range(1, MAX_API_PAGES + 1):
        path = (
            f"repos/{urllib.parse.quote(api.repo_name, safe='/')}/actions/runs/"
            f"{run_id}/attempts/{attempt}/jobs"
        )
        data = api.rest(
            path,
            params={"per_page": 100, "page": page},
            warning_label=f"jobs for workflow run {run_id}, attempt {attempt}, page {page}",
        )
        if data is None:
            break
        raw_pages.append(data)
        _write_raw_api(output_dir / f"page-{page}.json", data)
        if not isinstance(data, dict):
            break
        page_jobs = data.get("jobs")
        if not isinstance(page_jobs, list):
            break
        jobs.extend(item for item in page_jobs if isinstance(item, dict))
        if len(page_jobs) < 100:
            complete = True
            break
        if page == MAX_API_PAGES:
            warnings.append(
                {
                    "category": "api_page_limit",
                    "message": (
                        f"Job listing for run {run_id} reached the "
                        f"{MAX_API_PAGES}-page limit"
                    ),
                }
            )
    return jobs, raw_pages, complete


def _collect_workflow_attempts(
    api: GitHubAPI,
    observed_runs: list[dict[str, Any]],
    source_run: dict[str, Any],
    output_dir: Path,
    warnings: list[dict[str, str]],
) -> tuple[list[dict[str, Any]], dict[tuple[int, int], list[dict[str, Any]]]]:
    """Collect each observed target workflow's bounded attempt history.

    A GitHub run's ``run_attempt=N`` exposes attempts 1 through N under the
    same run ID. Each attempt's metadata and jobs are retained separately;
    job-list completeness is recorded independently of attempt metadata, so
    available jobs can still establish cost completeness when metadata is
    missing. Deduplicating run IDs before visiting attempts prevents duplicate
    runner cost from the listing and completion webhook.
    """
    jobs_by_run = {}
    source_id = source_run.get("id")
    source_attempt = int(source_run.get("run_attempt") or 1)
    latest_by_id: dict[int, dict[str, Any]] = {}
    for run in observed_runs:
        if run.get("name") not in TARGET_WORKFLOWS:
            continue
        run_id = run.get("id")
        if not isinstance(run_id, int):
            warnings.append(
                {
                    "category": "missing_run_id",
                    "message": f"Observed workflow {run.get('name')} had no run id",
                }
            )
            continue
        existing = latest_by_id.get(run_id)
        if existing is None or int(run.get("run_attempt") or 1) > int(
            existing.get("run_attempt") or 1
        ):
            latest_by_id[run_id] = run
    if isinstance(source_id, int) and source_run.get("name") in TARGET_WORKFLOWS:
        existing = latest_by_id.get(source_id)
        if existing is None or source_attempt >= int(existing.get("run_attempt") or 1):
            latest_by_id[source_id] = source_run

    attempts: list[dict[str, Any]] = []
    for run_id, latest in sorted(latest_by_id.items()):
        attempt_count = max(1, int(latest.get("run_attempt") or 1))
        for attempt in range(1, attempt_count + 1):
            key = (run_id, attempt)
            detail = api.workflow_run_attempt(run_id, attempt)
            _write_raw_api(
                output_dir / "run-attempts" / f"run-{run_id}-attempt-{attempt}.json", detail
            )
            if isinstance(detail, dict):
                record = dict(detail)
                record["attempt_metadata_source"] = "workflow_run_attempt_api"
            elif attempt == attempt_count or (run_id == source_id and attempt == source_attempt):
                fallback_run = (
                    source_run
                    if run_id == source_id and attempt == source_attempt
                    else latest
                )
                record = dict(fallback_run)
                record["attempt_metadata_source"] = (
                    "source_workflow_run_event"
                    if run_id == source_id and attempt == source_attempt
                    else "workflow_runs_listing"
                )
                if not (run_id == source_id and attempt == source_attempt):
                    warnings.append(
                        {
                            "category": "workflow_attempt_details_unavailable",
                            "message": (
                                f"Could not fetch metadata for latest workflow run {run_id} "
                                f"attempt {attempt}; used its observed run record"
                            ),
                        }
                    )
            else:
                record = {
                    key_name: latest.get(key_name)
                    for key_name in (
                        "name", "workflow_id", "run_number", "event", "head_branch",
                        "head_sha", "created_at", "html_url", "check_suite_id",
                    )
                }
                record.update(
                    {
                        "id": run_id,
                        "run_attempt": attempt,
                        "status": "unavailable",
                        "conclusion": None,
                        "run_started_at": None,
                        "updated_at": None,
                        "attempt_metadata_source": "unavailable",
                    }
                )
                warnings.append(
                    {
                        "category": "historical_workflow_attempt_unavailable",
                        "message": (
                            f"Historical workflow run {run_id} attempt {attempt} was "
                            "not available from the Actions API"
                        ),
                    }
                )
            record["id"] = run_id
            record["run_attempt"] = attempt
            record["attempt_metadata_available"] = (
                detail is not None
                or attempt == attempt_count
                or (run_id == source_id and attempt == source_attempt)
            )
            attempts.append(record)

            attempt_jobs, _, jobs_complete = _api_job_pages(
                api, run_id, attempt,
                output_dir / "jobs" / f"run-{run_id}-attempt-{attempt}", warnings,
            )
            jobs_by_run[key] = attempt_jobs
            record["jobs_complete"] = jobs_complete
            if record["jobs_complete"] is not True:
                warnings.append(
                    {
                        "category": "workflow_attempt_jobs_unavailable",
                        "message": (
                            f"Jobs for workflow run {run_id} attempt {attempt} "
                            "were unavailable or incomplete"
                        ),
                    }
                )
    # Raw attempt responses are already archived. The normalized history keeps
    # only identity/result/timing and the evidence needed to diagnose cost gaps.
    fields = (
        "id", "run_attempt", "run_number", "name", "event", "head_branch",
        "head_sha", "status", "conclusion", "created_at", "run_started_at",
        "updated_at", "html_url", "attempt_metadata_source",
        "attempt_metadata_available", "jobs_complete",
    )
    attempts = [{field: run.get(field) for field in fields} for run in attempts]
    attempts.sort(
        key=lambda run: (
            run.get("name") or "",
            int(run.get("run_number") or 0),
            int(run.get("id") or 0),
            int(run.get("run_attempt") or 0),
        )
    )
    return attempts, jobs_by_run


def _graphql_response_warning(
    response: Any, warnings: list[dict[str, str]], *, label: str
) -> None:
    """Record a warning for partial/unavailable GraphQL evidence."""
    status, errors = graphql_response_status(response)
    if status == "partial":
        warnings.append(
            {
                "category": "graphql_partial",
                "message": f"{label} returned usable data with {len(errors)} GraphQL error(s)",
            }
        )
    elif status == "unavailable" and isinstance(response, dict):
        warnings.append(
            {
                "category": "graphql_unavailable",
                "message": f"{label} returned no usable GraphQL data",
            }
        )


def _collect_queue_observation(
    api: GitHubAPI,
    candidate_pr: int | None,
    queue_branch: str | None,
    queue_branch_source: str,
    output_dir: Path,
    warnings: list[dict[str, str]],
) -> dict[str, Any]:
    """Collect independent trunk topology and optional candidate entry evidence.

    Candidate-entry and repository-queue GraphQL requests are independent and
    each raw response is retained. Repository-level topology is collected
    whenever ``queue_branch`` is known, even when candidate ownership is
    unresolved. A unique repository entry is reconciled with the PR-specific
    entry; identity conflicts, mutable queue-state drift, and partial GraphQL
    statuses remain distinct. Absence is reported only when a resolved PR has
    no match in a complete repository snapshot. If either observation is
    unavailable or the repository snapshot is truncated, a null entry remains
    unresolved. Candidate entry data is collected only for a resolved PR.
    """
    raw_dir = output_dir / "api" / "queue"
    candidate_observed_at = utc_now()
    candidate_response = (
        api.candidate_queue_entry_graphql(candidate_pr)
        if isinstance(candidate_pr, int)
        else None
    )
    repository_observed_at = utc_now()
    repository_response = (
        api.repository_merge_queue_graphql(queue_branch)
        if isinstance(queue_branch, str) and queue_branch
        else None
    )
    _write_raw_api(raw_dir / "candidate-entry.json", candidate_response)
    _write_raw_api(raw_dir / "repository-queue.json", repository_response)
    _graphql_response_warning(
        candidate_response, warnings, label="candidate merge-queue entry lookup"
    )
    _graphql_response_warning(
        repository_response, warnings, label="repository merge-queue topology lookup"
    )
    candidate_queue = normalize_merge_queue_response(candidate_response)
    repository_queue = normalize_merge_queue_response(repository_response)
    entry_reconciliation = reconcile_candidate_queue_entry(
        candidate_queue,
        repository_queue,
        candidate_pr,
        candidate_observed_at=candidate_observed_at,
        repository_observed_at=repository_observed_at,
    )
    effective_entry = entry_reconciliation.get("effective_candidate_queue_entry")
    queue = {
        "queue_entry_status": entry_reconciliation.get("queue_entry_status", "unresolved"),
        "entry": effective_entry,
        **entry_reconciliation,
        "merge_queue": repository_queue.get("merge_queue"),
        "is_in_merge_queue": candidate_queue.get("is_in_merge_queue"),
        "graphql_available": bool(
            candidate_queue.get("graphql_available")
            or repository_queue.get("graphql_available")
        ),
        "candidate_graphql_status": candidate_queue.get("graphql_status"),
        "candidate_graphql_errors": candidate_queue.get("graphql_errors", []),
        "repository_graphql_status": repository_queue.get("graphql_status"),
        "repository_graphql_errors": repository_queue.get("graphql_errors", []),
    }
    queue["queue_branch"] = queue_branch
    queue["queue_branch_source"] = queue_branch_source
    if isinstance(candidate_pr, int) and queue.get("queue_entry_status") == "unresolved":
        warnings.append(
            {
                "category": "merge_queue_unresolved",
                "message": (
                    "The merge-queue GraphQL response did not provide a usable pull request"
                ),
            }
        )
    merge_queue = queue.get("merge_queue")
    if queue_branch and merge_queue is None:
        warnings.append(
            {
                "category": "merge_queue_snapshot_unavailable",
                "message": f"Repository merge queue for {queue_branch} was unavailable",
            }
        )
    if isinstance(merge_queue, dict) and merge_queue.get("truncated") is True:
        warnings.append(
            {
                "category": "queue_snapshot_truncated",
                "message": (
                    "The bounded merge-queue snapshot reached its page limit; totalCount "
                    "and pageInfo were preserved"
                ),
            }
        )
    return queue




def _latest_runs_by_name(runs: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    grouped: dict[str, list[dict[str, Any]]] = {}
    for run in runs:
        name = run.get("name") or run.get("workflow_name")
        if isinstance(name, str):
            grouped.setdefault(name, []).append(run)
    latest: dict[str, dict[str, Any]] = {}
    for name, candidates in grouped.items():
        latest[name] = max(
            candidates,
            key=lambda item: (
                item.get("created_at") or "",
                int(item.get("run_number") or 0),
                int(item.get("run_attempt") or 0),
                int(item.get("id") or 0),
            ),
        )
    return latest


def _safe_markdown(value: Any) -> str:
    text = "—" if value is None or value == "" else str(value)
    return (
        text.replace("\\", "\\\\")
        .replace("|", "\\|")
        .replace("\r", " ")
        .replace("\n", " ")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
    )

def _format_duration(value: Any) -> str:
    if not isinstance(value, (int, float)):
        return "—"
    return f"{value:.1f}s"


def _warning_text(warnings: list[dict[str, str]]) -> str:
    if not warnings:
        return "None"
    return "; ".join(
        _safe_markdown(warning.get("message"))
        for warning in warnings[:5]
    )






def expected_stack_members(stack: dict[str, Any], candidate_pr: int | None) -> list[dict[str, Any]] | None:
    """Select only this candidate's own stack positions 1..P for ancestry proof.

    Membership must come from a complete candidate lookup. Missing/duplicate
    positions, mismatched candidate number, or malformed identities stay
    unresolved. Repository queue entries are not inputs, preventing sister
    stacks from contributing members to the proof.
    """
    if stack.get("stack_status") != "stack_member" or candidate_pr is None:
        return None
    position = stack["stack_position"]
    entries = (stack.get("observed_stack") or {}).get("entries") or {}
    nodes = entries.get("nodes") if isinstance(entries, dict) else None
    if not isinstance(nodes, list):
        return None
    members = {}
    for node in nodes:
        if not isinstance(node, dict) or not isinstance(node.get("position"), int):
            return None
        index = node["position"]
        if index > position:
            continue
        pr = node.get("pullRequest")
        if (index < 1 or index in members or not isinstance(pr, dict)
            or not isinstance(pr.get("number"), int)
            or not re.fullmatch(r"[0-9a-fA-F]{40,64}", str(pr.get("headRefOid")))):
            return None
        members[index] = {
            "stack_position": index, "pr_number": pr["number"],
            "head_sha": pr["headRefOid"], "base_sha": pr.get("baseRefOid"),
        }
    if set(members) != set(range(1, position + 1)) or members[position]["pr_number"] != candidate_pr:
        return None
    if len({member["pr_number"] for member in members.values()}) != position:
        return None
    return [members[index] for index in range(1, position + 1)]


def _stack_composition(
    repo: Path, head_sha: str, candidate_pr: int | None, stack: dict[str, Any],
    output_dir: Path, warnings: list[dict[str, str]],
) -> dict[str, Any]:
    """Check expected own-stack heads with a single bounded, data-only Git fetch.

    Positive ancestry proves that head is contained. A negative result in
    shallow history stays unresolved; rewritten/squashed commits cannot be
    proven by ancestry alone. Ordinary/unresolved candidates do no Git scanning.
    """
    members = expected_stack_members(stack, candidate_pr)
    if members is None:
        return {
            "status": "not_applicable"
            if stack.get("stack_status") == "not_a_stack" else "unresolved",
            "members": [],
        }
    if not re.fullmatch(r"[0-9a-fA-F]{40,64}", head_sha):
        return {"status": "unresolved", "members": members}
    objects = list(dict.fromkeys([head_sha, *(member["head_sha"] for member in members)]))
    fetched = _git(
        repo, bounded_git_fetch_args(objects[0]) + objects[1:], warnings,
        label="bounded own-stack fetch",
    )
    shallow = _git(repo, ["rev-parse", "--is-shallow-repository"], warnings, label="history completeness")
    history_shallow = shallow.strip() == "true" if shallow is not None else None
    for member in members:
        member["head_is_ancestor"] = _git_is_ancestor(
            repo, member["head_sha"], head_sha, warnings, history_shallow=history_shallow
        )
    tree = _git(repo, ["rev-parse", head_sha + "^{tree}"], warnings, label="candidate tree")
    evidence = {
        "status": "proven" if all(member["head_is_ancestor"] is True for member in members) else "unresolved",
        "stack_id": stack["stack_id"], "candidate_pr": candidate_pr,
        "candidate_sha": head_sha, "candidate_position": stack["stack_position"],
        "members": members, "fetch_objects": objects,
        "fetch_depth": GIT_FETCH_DEPTH, "fetch_succeeded": fetched is not None,
        "history_shallow": history_shallow, "tree_sha": tree.strip() if tree else None,
    }
    save_json(output_dir / "git" / "own-stack-composition.json", evidence)
    return evidence


def _collect_identity(
    api: GitHubAPI, head_sha: str, ref_signals: list[tuple[str, Any]],
    output_dir: Path, warnings: list[dict[str, str]],
) -> tuple[dict[str, Any], dict[str, Any] | None]:
    """Corroborate ref ownership, then query only that PR's own native stack.

    Head associations remain an independent ownership signal, not a generic
    composition search. No parent/introduced-commit or message scanning occurs.
    """
    associated = api.associated_pulls(head_sha)
    _write_raw_api(output_dir / "api" / "associated-pulls.json", associated)
    ref_numbers = {number for _, ref in ref_signals if (number := parse_queue_pr_number(ref)) is not None}
    metadata = {}
    for number in sorted(ref_numbers):
        value = api.pull_request(number)
        _write_raw_api(output_dir / "api" / "prs" / f"{number}.json", value)
        metadata[number] = value if isinstance(value, dict) else None
    identity = derive_candidate_identity(
        ref_signals=ref_signals, head_associated_pulls=associated, pull_requests=metadata
    )
    number = identity["candidate_pr"]
    candidate = metadata.get(number)
    if number is not None:
        response = api.stack_graphql(number)
        _write_raw_api(output_dir / "api" / "prs" / f"{number}-stack.json", response)
        _graphql_response_warning(response, warnings, label="candidate stack lookup")
        stack = classify_stack(response)
        graphql_pr, _, _ = _graphql_pull_request(response)
        if graphql_pr is not None and graphql_pr.get("number") != number:
            stack = _unresolved_stack("Stack response did not identify the candidate PR")
    else:
        stack = _unresolved_stack("Candidate PR ownership is unresolved")
    identity["stack"] = stack
    return identity, candidate


def _target_runs(
    api: GitHubAPI, head_sha: str, output_dir: Path, warnings: list[dict[str, str]]
) -> list[dict[str, Any]]:
    """Keep a bounded raw run listing, normalize only the three target workflows."""
    runs, _ = _api_run_pages(api, head_sha, output_dir / "api" / "actions" / "run-pages", warnings)
    return [run for run in runs if run.get("event") == "merge_group"
            and run.get("head_sha") == head_sha and run.get("name") in TARGET_WORKFLOWS]


def _snapshot(event_path: Path, repo: Path, output_dir: Path, step_summary: Path | None) -> dict[str, Any]:
    """Observe this candidate's identity, stack prefix, and current trunk queue."""
    warnings = []
    event = copy_event(event_path, output_dir)
    environment = safe_environment()
    save_json(output_dir / "environment.json", environment)
    group = event.get("merge_group")
    if not isinstance(group, dict) or not isinstance(group.get("head_sha"), str):
        raise ValueError("merge_group event is missing its candidate SHA")
    api = GitHubAPI(warnings)
    sha = group["head_sha"]
    identity, candidate = _collect_identity(api, sha, [
        ("merge_group.head_ref", group.get("head_ref")), ("GITHUB_REF", environment.get("GITHUB_REF"))
    ], output_dir, warnings)
    branch = resolve_queue_branch(
        merge_group_base_ref=group.get("base_ref"),
        stack=identity["stack"], pull_request=candidate,
    )
    queue = _collect_queue_observation(
        api, identity["candidate_pr"], branch["queue_branch"],
        branch["queue_branch_source"], output_dir, warnings,
    )
    runs = _target_runs(api, sha, output_dir, warnings)
    collected_at = utc_now()
    summary = {
        "mode": "merge_group_snapshot", "collected_at": collected_at,
        "action": event.get("action"), "merge_group": group, "identity": identity,
        "queue": queue,
        "composition": _stack_composition(
            repo, sha, identity["candidate_pr"], identity["stack"], output_dir, warnings
        ),
        "queue_timing": derive_queue_timing(collected_at, queue.get("entry"), runs, candidate_sha=sha),
        "target_workflow_observations": runs, "warnings": warnings,
    }
    _write_api_errors(api, output_dir)
    save_json(output_dir / "summary.json", summary)
    save_json(output_dir / "warnings.json", warnings)
    _write_snapshot_summary(summary, step_summary)
    return summary


def _timing(event_path: Path, output_dir: Path, step_summary: Path | None) -> dict[str, Any]:
    """Collect target siblings and all attempts without waiting for other runs."""
    warnings = []
    event = copy_event(event_path, output_dir)
    save_json(output_dir / "environment.json", safe_environment())
    source = event.get("workflow_run")
    if not isinstance(source, dict) or source.get("event") != "merge_group":
        raise ValueError("timing collector requires a merge_group workflow_run")
    sha = source.get("head_sha")
    if not isinstance(source.get("id"), int) or not isinstance(sha, str) or not sha:
        raise ValueError("workflow_run event is missing its run id or head SHA")
    api = GitHubAPI(warnings)
    runs = _target_runs(api, sha, output_dir, warnings)
    attempts, jobs = _collect_workflow_attempts(api, runs, source, output_dir / "api" / "actions", warnings)
    save_json(output_dir / "api" / "actions" / "workflow-attempts.json", attempts)
    for rows in jobs.values():
        for job in rows:
            if runner_metadata_missing_unexpectedly(job):
                warnings.append({
                    "category": "runner_metadata_missing",
                    "message": f"Runner classification unavailable for job {job.get('id')}",
                })
    identity, candidate = _collect_identity(
        api, sha, [("workflow_run.head_branch", source.get("head_branch"))],
        output_dir, warnings,
    )
    queue_branch = queue_branch_from_queue_ref(source.get("head_branch"))
    branch = ({"queue_branch": queue_branch, "queue_branch_source": "workflow_run.head_branch"}
              if queue_branch else resolve_queue_branch(stack=identity["stack"], pull_request=candidate))
    queue = _collect_queue_observation(
        api, identity["candidate_pr"], branch["queue_branch"],
        branch["queue_branch_source"], output_dir, warnings,
    )
    collected_at = utc_now()
    summary = {
        "mode": "workflow_completion_timing", "collected_at": collected_at,
        "candidate_head_sha": sha, "candidate_queue_ref": source.get("head_branch"),
        "identity": identity, "queue": queue,
        "queue_timing": derive_queue_timing(collected_at, queue.get("entry"), attempts, candidate_sha=sha),
        "observed_sibling_workflows": attempts,
        "timing": derive_timing_metrics(_latest_runs_by_name(attempts), jobs, attempts),
        "warnings": warnings,
    }
    _write_api_errors(api, output_dir)
    save_json(output_dir / "summary.json", summary)
    save_json(output_dir / "warnings.json", warnings)
    _write_timing_summary(summary, step_summary)
    return summary


def _identity_summary_lines(summary: dict[str, Any]) -> list[str]:
    """Render candidate-scoped identity and current entry provenance concisely."""
    identity = summary.get("identity") or {}
    stack = identity.get("stack") or {}
    queue = summary.get("queue") or {}
    entry = queue.get("entry") or {}
    return [
        f"**Candidate PR:** {_safe_markdown(identity.get('candidate_pr', 'unresolved'))}",
        f"**Ownership:** {_safe_markdown(identity.get('candidate_pr_resolution'))}",
        f"**Own stack:** {_safe_markdown(stack.get('stack_status'))}; "
        f"ID {_safe_markdown(stack.get('stack_id'))}; "
        f"position {_safe_markdown(stack.get('stack_position'))} / "
        f"{_safe_markdown(stack.get('stack_size'))}",
        f"**Trunk queue:** {_safe_markdown(queue.get('queue_branch'))}; "
        f"entry {_safe_markdown(queue.get('queue_entry_status'))} "
        f"({_safe_markdown(queue.get('effective_candidate_queue_entry_source'))}); "
        f"queue position {_safe_markdown(entry.get('queue_entry_position'))}",
        f"**Enqueued at:** {_safe_markdown(entry.get('enqueued_at'))}",
    ]


def _write_snapshot_summary(summary: dict[str, Any], destination: Path | None) -> None:
    """Show ownership and own-stack proof; full bounded queue stays in artifacts."""
    if destination is None:
        return
    group = summary["merge_group"]
    timing = summary["queue_timing"]
    lines = [
        "## Merge queue telemetry", "",
        f"**Candidate SHA:** `{_safe_markdown(group.get('head_sha'))}`",
        f"**Queue ref:** {_safe_markdown(group.get('head_ref'))}",
        f"**Raw base ref / SHA:** {_safe_markdown(group.get('base_ref'))} / "
        f"{_safe_markdown(group.get('base_sha'))}",
        *_identity_summary_lines(summary),
        f"**Own-stack composition:** {_safe_markdown(summary['composition']['status'])}",
        f"**Queue age:** {_format_duration(timing['queue_age_at_snapshot_seconds'])}",
        "**Queue → first target dispatch:** "
        f"{_format_duration(timing['queue_to_first_target_workflow_created_seconds'])}",
        f"**Warnings:** {_warning_text(summary['warnings'])}", "",
    ]
    destination.write_text("\n\n".join(lines), encoding="utf-8")


def _write_timing_summary(summary: dict[str, Any], destination: Path | None) -> None:
    """Show latest workflow results and observed all-attempt runner-cost coverage."""
    if destination is None:
        return
    timing = summary["timing"]
    lines = [
        "## Merge queue timing", "", *_identity_summary_lines(summary), "",
        "| Workflow | Run / attempt | Start delay | Runtime | Result |",
        "| --- | --- | --- | --- | --- |",
    ]
    for row in timing["workflow_rows"]:
        lines.append(
            f"| {_safe_markdown(row['workflow_name'])} | "
            f"{_safe_markdown(row.get('run_id'))} / {_safe_markdown(row.get('run_attempt'))} | "
            f"{_format_duration(row['workflow_created_to_run_started_seconds'])} | "
            f"{_format_duration(row['runtime_seconds'])} | "
            f"{_safe_markdown(row.get('conclusion') or row.get('status'))} |"
        )
    runner_time = timing["runner_time_seconds"]
    lines.extend([
        "", f"**Latest workflows:** {timing['timing_status']}",
        f"**Candidate wall time:** {_format_duration(timing['candidate_wall_time_seconds'])}",
        "**Observed runner time (all attempts):** "
        f"self-hosted {_format_duration(runner_time['self-hosted'])}; "
        f"GitHub-hosted {_format_duration(runner_time['github-hosted'])}; "
        f"unknown {_format_duration(runner_time['unknown'])}",
        f"**Runner-cost accounting:** {timing['runner_time_status']}",
    ])
    missing = timing["runner_time_missing_attempts"]
    if missing:
        lines.append("Missing runner-cost evidence (totals are a lower bound): " + "; ".join(
            f"{_safe_markdown(item['workflow_name'])} run {item['run_id']} / "
            f"attempt {item['run_attempt']} ({_safe_markdown(item['reason'])})"
            for item in missing
        ))
    queue_timing = summary.get("queue_timing") or {}
    for field, label in (
        ("queue_to_first_target_workflow_created_seconds", "Queue → first target dispatch"),
        ("queue_to_first_target_workflow_started_seconds", "Queue → first target start"),
        ("queue_to_target_workflows_complete_seconds", "Queue → latest target workflows completed"),
    ):
        lines.append(f"**{label}:** {_format_duration(queue_timing.get(field))}")
    lines.extend([f"**Warnings:** {_warning_text(summary.get('warnings', []))}", ""])
    destination.write_text("\n".join(lines), encoding="utf-8")


def main(argv: list[str] | None = None) -> int:
    """Dispatch the candidate snapshot or target-workflow timing collector."""
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="mode", required=True)
    for mode in ("snapshot", "timing"):
        subparser = subparsers.add_parser(mode)
        subparser.add_argument("--event", required=True, type=Path)
        subparser.add_argument("--output", required=True, type=Path)
        if mode == "snapshot":
            subparser.add_argument("--repo", required=True, type=Path)
    args = parser.parse_args(argv)
    summary_path = (
        Path(os.environ["GITHUB_STEP_SUMMARY"])
        if os.environ.get("GITHUB_STEP_SUMMARY")
        else None
    )

    if args.mode == "snapshot":
        result = _snapshot(args.event, args.repo, args.output, summary_path)
    else:
        result = _timing(args.event, args.output, summary_path)
    if args.mode == "timing":
        timing = result.get("timing") if isinstance(result, dict) else None
        status = (
            timing.get("timing_status")
            if isinstance(timing, dict)
            and timing.get("timing_status") in {"complete", "partial"}
            else "timing status unavailable"
        )
    else:
        status = "snapshot captured"
    warnings = result.get("warnings") if isinstance(result, dict) else None
    warning_count = len(warnings) if isinstance(warnings, list) else 0
    print(
        f"Merge queue telemetry {args.mode} completed: "
        f"{status}; {warning_count} warning(s)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
