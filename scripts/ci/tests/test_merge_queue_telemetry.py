"""Objective-focused fixtures: ownership, own-stack isolation/composition, and cost.

All fake APIs/fixtures stay here. Read-only/trusted-master workflow structure,
partial evidence, queue reconciliation, bounded ancestry, and retry cost are
regressed independently of any future CI decision. Run from the repository root:
``python3 -m unittest discover -s scripts/ci/tests -p "test*.py"``.
"""

from pathlib import Path
import re
from unittest.mock import patch
import tempfile
import unittest

from scripts.ci.merge_queue_telemetry import (
    GIT_FETCH_DEPTH, TARGET_WORKFLOWS, ancestry_from_exit_code,
    bounded_git_fetch_args, classify_stack, classify_runner,
    derive_candidate_identity, derive_queue_timing, derive_timing_metrics,
    duration_seconds, expected_stack_members, normalize_api_error,
    normalize_merge_queue_response, normalize_branch_ref, parse_queue_pr_number,
    queue_branch_from_queue_ref, reconcile_candidate_queue_entry,
    resolve_queue_branch, runner_metadata_missing_unexpectedly,
    _collect_identity, _collect_queue_observation, _collect_workflow_attempts,
    _latest_runs_by_name, _snapshot, _stack_composition, _write_timing_summary, GitHubAPI,
)


def graphql_pr(stack=None, entry=None):
    if isinstance(entry, dict) and isinstance(stack, dict):
        entry = {**entry, "stack": {"id": stack.get("id")}}
    return {
        "data": {
            "repository": {
                "pullRequest": {
                    "number": 101,
                    "stack": stack,
                    "stackEntry": entry,
                }
            }
        }
    }


def merge_queue_response(
    *,
    entry=True,
    total_count=1,
    has_next_page=False,
    nodes=None,
):
    if nodes is None:
        nodes = [
            {
                "id": "entry-101",
                "enqueuedAt": "2026-01-01T00:00:00Z",
                "position": 2,
                "state": "AWAITING_CHECKS",
                "baseCommit": {"oid": "base-sha"},
                "headCommit": {"oid": "candidate-sha"},
                "pullRequest": {
                    "number": 101,
                    "baseRefName": "master",
                    "baseRefOid": "base-sha",
                    "headRefName": "feature-101",
                    "headRefOid": "pr-head-sha",
                    "stack": None,
                    "stackEntry": None,
                },
            }
        ]
    queue_entry = nodes[0] if entry and nodes else None
    queue = {
        "id": "queue-1",
        "entries": {
            "totalCount": total_count,
            "pageInfo": {
                "hasNextPage": has_next_page,
                "endCursor": "cursor-1" if has_next_page else None,
            },
            "nodes": nodes,
        },
    }
    return {
        "data": {
            "repository": {
                "mergeQueue": queue,
                "pullRequest": {
                    "number": 101,
                    "isInMergeQueue": bool(entry),
                    "mergeQueueEntry": queue_entry,
                }
            }
        }
    }


class FakeQueueAPI:
    """Capture split queue calls while returning bounded synthetic responses."""

    def __init__(self, candidate=None, repository=None):
        self.candidate = candidate
        self.repository = repository
        self.calls = []

    def candidate_queue_entry_graphql(self, number):
        self.calls.append(("candidate", number))
        return self.candidate

    def repository_merge_queue_graphql(self, branch):
        self.calls.append(("repository", branch))
        return self.repository


class FakeAttemptAPI:
    """Serve attempt metadata and attempt-scoped jobs without network access."""

    repo_name = "owner/repo"

    def __init__(self, details, jobs):
        self.details = details
        self.jobs = jobs
        self.attempt_calls = []
        self.job_calls = []

    def workflow_run_attempt(self, run_id, attempt):
        self.attempt_calls.append((run_id, attempt))
        return self.details.get((run_id, attempt))

    def rest(self, path, *, params=None, warning_label=None):
        parts = path.split("/")
        runs_index = parts.index("runs")
        run_id = int(parts[runs_index + 1])
        attempt = int(parts[runs_index + 3])
        self.job_calls.append((run_id, attempt))
        if (run_id, attempt) not in self.jobs:
            return None
        return {"jobs": self.jobs[(run_id, attempt)]}


def workflow_attempt_record(
    run_id, attempt, name, conclusion, started_at, completed_at, created_at
):
    return {
        "id": run_id,
        "run_attempt": attempt,
        "run_number": run_id,
        "workflow_id": run_id + 1000,
        "name": name,
        "event": "merge_group",
        "head_sha": "candidate-sha",
        "head_branch": "gh-readonly-queue/master/pr-101-abcdef",
        "status": "completed",
        "conclusion": conclusion,
        "created_at": created_at,
        "run_started_at": started_at,
        "updated_at": completed_at,
    }


def attempt_job(name, conclusion, started_at, completed_at, workflow_name):
    return {
        "id": f"{workflow_name}-{name}-{started_at}",
        "name": name,
        "workflow_name": workflow_name,
        "status": "completed",
        "conclusion": conclusion,
        "started_at": started_at,
        "completed_at": completed_at,
        "runner_name": "runner-linux-1",
        "runner_group_name": "self-hosted-linux",
        "labels": ["self-hosted", "Linux", "X64"],
    }


class QueryCaptureAPI(GitHubAPI):
    """Capture generated GraphQL documents without making network requests."""

    def __init__(self):
        super().__init__([])
        self.owner = "owner"
        self.repo = "repo"
        self.queries = []

    def graphql(self, query, variables, warning_label):
        self.queries.append((query, variables, warning_label))
        return {"data": {}}


class QueueReferenceTests(unittest.TestCase):
    def test_normalizes_only_branch_namespace_refs(self):
        for raw, expected in (
            ("refs/heads/master", "master"),
            ("refs/heads/release/0.4", "release/0.4"),
            ("master", "master"),
            ("release/0.4", "release/0.4"),
            ("refs/tags/v1", None),
        ):
            self.assertEqual(normalize_branch_ref(raw), expected)

    def test_parses_queue_ref_pr_number(self):
        self.assertEqual(
            parse_queue_pr_number(
                "refs/heads/gh-readonly-queue/master/pr-3716-a1b2c3d"
            ),
            3716,
        )

    def test_does_not_infer_from_ordinary_branch(self):
        self.assertIsNone(parse_queue_pr_number("feature/pr-3716-a1b2c3d"))
        self.assertIsNone(parse_queue_pr_number("refs/heads/gh-readonly-queue/master"))

    def test_queue_branch_parser_preserves_slashes_in_base_branch(self):
        self.assertEqual(
            queue_branch_from_queue_ref("gh-readonly-queue/release/0.4/pr-123-abcde"),
            "release/0.4",
        )
        self.assertEqual(
            queue_branch_from_queue_ref(
                "gh-readonly-queue/team/pr-123-experiment/pr-456-abcdef"
            ),
            "team/pr-123-experiment",
        )
        self.assertEqual(
            parse_queue_pr_number(
                "gh-readonly-queue/team/pr-123-experiment/pr-456-abcdef"
            ),
            456,
        )
        self.assertEqual(
            parse_queue_pr_number("gh-readonly-queue/release/0.4/pr-123-abcde"),
            123,
        )
        self.assertIsNone(queue_branch_from_queue_ref("refs/heads/feature/test"))


class CandidateIdentityTests(unittest.TestCase):

    def test_single_pr_with_matching_queue_and_api_evidence(self):
        identity = derive_candidate_identity(
            ref_signals=[
                ("GITHUB_REF_queue_ref", "refs/heads/gh-readonly-queue/master/pr-101-a1b2c3d"),
                ("merge_group.head_ref", "gh-readonly-queue/master/pr-101-a1b2c3d"),
            ],
            head_associated_pulls=[{"number": 101}, {"number": 99}],

            pull_requests={101: {"number": 101}},
        )
        self.assertEqual(identity["candidate_pr"], 101)
        self.assertEqual(identity["candidate_pr_confidence"], "corroborated")
        self.assertEqual(identity["candidate_pr_candidates"], [99, 101])

    def test_ambiguous_association_is_unresolved_without_queue_ref(self):
        identity = derive_candidate_identity(
            ref_signals=[],
            head_associated_pulls=[{"number": 101}, {"number": 102}],

            pull_requests={101: {"number": 101}, 102: {"number": 102}},
        )
        self.assertIsNone(identity["candidate_pr"])
        self.assertEqual(identity["candidate_pr_confidence"], "unresolved")
        self.assertEqual(identity["candidate_pr_candidates"], [101, 102])

    def test_ref_and_head_association_conflict_remains_unresolved(self):
        identity = derive_candidate_identity(
            ref_signals=[
                ("merge_group.head_ref", "gh-readonly-queue/master/pr-101-a1b2c3d")
            ],
            head_associated_pulls=[{"number": 102}],

            pull_requests={101: {"number": 101}, 102: {"number": 102}},
        )
        self.assertIsNone(identity["candidate_pr"])

    def test_queue_ref_alone_is_not_authoritative(self):
        identity = derive_candidate_identity(
            ref_signals=[
                ("merge_group.head_ref", "gh-readonly-queue/master/pr-101-a1b2c3d")
            ],
            head_associated_pulls=[],

            pull_requests={101: {"number": 101}},
        )
        self.assertIsNone(identity["candidate_pr"])
        self.assertEqual(identity["candidate_pr_candidates"], [101])
        self.assertEqual(identity["candidate_pr_confidence"], "unresolved")


class QueueObservationTests(unittest.TestCase):
    def test_normalized_merge_group_base_is_used_for_repository_queue_query(self):
        for raw, expected in (
            ("refs/heads/master", "master"),
            ("refs/heads/release/0.4", "release/0.4"),
        ):
            branch_info = resolve_queue_branch(merge_group_base_ref=raw)
            self.assertEqual(branch_info["queue_branch"], expected)
            self.assertEqual(branch_info["queue_branch_source"], "merge_group.base_ref")
            repository = merge_queue_response()
            repository["data"]["repository"].pop("pullRequest")
            api = FakeQueueAPI(repository=repository)
            with tempfile.TemporaryDirectory() as temporary:
                _collect_queue_observation(
                    api,
                    None,
                    branch_info["queue_branch"],
                    branch_info["queue_branch_source"],
                    Path(temporary),
                    [],
                )
            self.assertIn(("repository", expected), api.calls)


    def test_null_entry_is_absent_without_losing_repository_topology(self):
        result = normalize_merge_queue_response(merge_queue_response(entry=False))
        self.assertEqual(result["queue_entry_status"], "absent")
        self.assertIsNone(result["entry"])
        self.assertEqual(result["merge_queue"]["id"], "queue-1")

    def test_graphql_failure_is_unresolved(self):
        result = normalize_merge_queue_response(
            {"errors": [{"message": "field unavailable"}]}
        )
        self.assertEqual(result["queue_entry_status"], "unresolved")
        self.assertFalse(result["graphql_available"])

    def test_partial_graphql_queue_data_is_retained(self):
        response = merge_queue_response()
        response["errors"] = [{"message": "unrelated preview field", "path": ["x"]}]
        result = normalize_merge_queue_response(response)
        self.assertEqual(result["graphql_status"], "partial")
        self.assertEqual(result["merge_queue"]["id"], "queue-1")
        self.assertEqual(result["graphql_errors"][0]["message"], "unrelated preview field")


    def test_candidate_entry_reconciliation_sources_and_conflicts(self):
        candidate = normalize_merge_queue_response(merge_queue_response())
        repository = normalize_merge_queue_response(merge_queue_response())
        corroborated = reconcile_candidate_queue_entry(candidate, repository, 101)
        self.assertEqual(corroborated["effective_candidate_queue_entry_source"], "corroborated")
        repository["merge_queue"]["entries"] = []
        from_repository = reconcile_candidate_queue_entry(
            {"entry": None}, normalize_merge_queue_response(merge_queue_response()), 101
        )
        self.assertEqual(from_repository["effective_candidate_queue_entry_source"], "repository.mergeQueue.entries")
        missing = reconcile_candidate_queue_entry(candidate, repository, 101)
        self.assertEqual(missing["effective_candidate_queue_entry_source"], "pull_request.mergeQueueEntry")
        no_match = reconcile_candidate_queue_entry(
            {"entry": None}, repository, 999
        )
        self.assertEqual(no_match["effective_candidate_queue_entry_source"], "unresolved")
        duplicate = normalize_merge_queue_response(merge_queue_response(nodes=[
            merge_queue_response()["data"]["repository"]["mergeQueue"]["entries"]["nodes"][0],
            {**merge_queue_response()["data"]["repository"]["mergeQueue"]["entries"]["nodes"][0], "id": "entry-duplicate"},
        ], total_count=2))
        ambiguous = reconcile_candidate_queue_entry({"entry": None}, duplicate, 101)
        self.assertTrue(ambiguous["candidate_entry_identity_conflict"])
        self.assertEqual(ambiguous["effective_candidate_queue_entry_source"], "unresolved")
        candidate_raw = merge_queue_response()
        candidate_raw["data"]["repository"]["pullRequest"]["mergeQueueEntry"]["position"] = 3
        candidate_raw["data"]["repository"]["pullRequest"]["mergeQueueEntry"]["state"] = "AWAITING_CHECKS"
        repository_raw = merge_queue_response()
        drift_node = repository_raw["data"]["repository"]["mergeQueue"]["entries"]["nodes"][0]
        drift_node["position"] = 2
        drift_node["state"] = "BUILDING"
        drift = reconcile_candidate_queue_entry(
            normalize_merge_queue_response(candidate_raw),
            normalize_merge_queue_response(repository_raw),
            101,
            candidate_observed_at="2026-01-01T00:00:00Z",
            repository_observed_at="2026-01-01T00:00:01Z",
        )
        self.assertFalse(drift["candidate_entry_identity_conflict"])
        self.assertEqual(
            set(drift["candidate_entry_observation_drift_fields"]),
            {"queue_entry_position", "state", },
        )
        self.assertEqual(drift["effective_candidate_queue_entry"]["queue_entry_position"], 2)
        self.assertEqual(drift["effective_candidate_queue_entry"]["state"], "BUILDING")
        self.assertEqual(
            drift["candidate_entry_from_pull_request_observed_at"],
            "2026-01-01T00:00:00Z",
        )
        self.assertEqual(
            drift["candidate_entry_from_repository_queue_observed_at"],
            "2026-01-01T00:00:01Z",
        )

        for field, candidate_value, repository_value in (
            ("id", "entry-pr", "entry-repository"),
            ("headCommit", {"oid": "pr-head"}, {"oid": "repo-head"}),
        ):
            left = merge_queue_response()
            right = merge_queue_response()
            left["data"]["repository"]["pullRequest"]["mergeQueueEntry"][field] = candidate_value
            right["data"]["repository"]["mergeQueue"]["entries"]["nodes"][0][field] = repository_value
            conflict = reconcile_candidate_queue_entry(
                normalize_merge_queue_response(left),
                normalize_merge_queue_response(right),
                101,
            )
            self.assertTrue(conflict["candidate_entry_identity_conflict"])
            self.assertIsNone(conflict["effective_candidate_queue_entry"])

    def test_partial_candidate_entry_is_retained(self):
        response = merge_queue_response()
        response["errors"] = [{"message": "stack preview unavailable"}]
        result = normalize_merge_queue_response(response)
        self.assertEqual(result["graphql_status"], "partial")
        self.assertEqual(result["entry"]["id"], "entry-101")

    def test_partial_error_on_candidate_entry_does_not_claim_absence(self):
        response = merge_queue_response(entry=False)
        response["errors"] = [
            {"message": "entry field unavailable", "path": ["repository", "pullRequest", "mergeQueueEntry"]}
        ]
        normalized = normalize_merge_queue_response(response)
        self.assertEqual(normalized["queue_entry_status"], "unresolved")

    @staticmethod
    def empty_repository_queue(*, truncated=False):
        response = merge_queue_response(
            entry=False,
            total_count=101 if truncated else 0,
            has_next_page=truncated,
            nodes=[],
        )
        response["data"]["repository"].pop("pullRequest")
        return response

    def test_candidate_entry_absence_requires_complete_queue_evidence(self):
        with tempfile.TemporaryDirectory() as temporary:
            complete = _collect_queue_observation(
                FakeQueueAPI(
                    candidate=merge_queue_response(entry=False, nodes=[]),
                    repository=self.empty_repository_queue(),
                ),
                101,
                "master",
                "merge_group.base_ref",
                Path(temporary),
                [],
            )
        self.assertEqual(complete["queue_entry_status"], "absent")
        self.assertFalse(complete["is_in_merge_queue"])

    def test_candidate_absence_with_unavailable_repository_is_unresolved(self):
        with tempfile.TemporaryDirectory() as temporary:
            observation = _collect_queue_observation(
                FakeQueueAPI(
                    candidate=merge_queue_response(entry=False, nodes=[]),
                    repository={"errors": [{"message": "queue unavailable"}]},
                ),
                101,
                "master",
                "merge_group.base_ref",
                Path(temporary),
                [],
            )
        self.assertEqual(observation["queue_entry_status"], "unresolved")
        self.assertEqual(observation["candidate_entry_from_pull_request"], None)
        self.assertEqual(observation["is_in_merge_queue"], False)

    def test_complete_repository_no_match_can_establish_absence_without_candidate_data(self):
        with tempfile.TemporaryDirectory() as temporary:
            observation = _collect_queue_observation(
                FakeQueueAPI(candidate=None, repository=self.empty_repository_queue()),
                101,
                "master",
                "merge_group.base_ref",
                Path(temporary),
                [],
            )
        self.assertEqual(observation["queue_entry_status"], "absent")

    def test_truncated_repository_no_match_does_not_establish_absence(self):
        with tempfile.TemporaryDirectory() as temporary:
            observation = _collect_queue_observation(
                FakeQueueAPI(
                    candidate=merge_queue_response(entry=False, nodes=[]),
                    repository=self.empty_repository_queue(truncated=True),
                ),
                101,
                "master",
                "merge_group.base_ref",
                Path(temporary),
                [],
            )
        self.assertEqual(observation["queue_entry_status"], "unresolved")

    def test_repository_queue_remains_available_when_candidate_is_unresolved(self):
        response = merge_queue_response()
        response["data"]["repository"].pop("pullRequest")
        result = normalize_merge_queue_response(response)
        self.assertEqual(result["queue_entry_status"], "unresolved")
        self.assertEqual(result["merge_queue"]["id"], "queue-1")


    def test_queue_snapshot_preserves_stacks_and_truncation(self):
        stack_a = {
            "id": "stack-a",
            "number": 7,
            "size": 2,
            "baseRefName": "master",
        }
        nodes = [
            {
                "id": "a1",
                "position": 1,
                "enqueuedAt": "2026-01-01T00:00:00Z",
                "headCommit": {"oid": "a1-sha"},
                "pullRequest": {
                    "number": 201,
                    "baseRefName": "master",
                    "baseRefOid": "base",
                    "headRefName": "a1",
                    "headRefOid": "a1-sha",
                    "stack": stack_a,
                    "stackEntry": {"position": 1},
                },
            },
            {
                "id": "a2",
                "position": 2,
                "enqueuedAt": "2026-01-01T00:01:00Z",
                "headCommit": {"oid": "a2-sha"},
                "pullRequest": {
                    "number": 202,
                    "baseRefName": "a1",
                    "baseRefOid": "a1-sha",
                    "headRefName": "a2",
                    "headRefOid": "a2-sha",
                    "stack": stack_a,
                    "stackEntry": {"position": 2},
                },
            },
            {
                "id": "b1",
                "position": 3,
                "enqueuedAt": "2026-01-01T00:02:00Z",
                "headCommit": {"oid": "b1-sha"},
                "pullRequest": {
                    "number": 301,
                    "baseRefName": "master",
                    "baseRefOid": "base",
                    "headRefName": "b1",
                    "headRefOid": "b1-sha",
                    "stack": {
                        "id": "stack-b",
                        "number": 8,
                        "size": 2,
                        "baseRefName": "master",
                    },
                    "stackEntry": {"position": 1},
                },
            },
            {
                "id": "ordinary",
                "position": 4,
                "enqueuedAt": "2026-01-01T00:03:00Z",
                "headCommit": {"oid": "ordinary-sha"},
                "pullRequest": {
                    "number": 401,
                    "baseRefName": "master",
                    "baseRefOid": "base",
                    "headRefName": "ordinary",
                    "headRefOid": "ordinary-sha",
                    "stack": None,
                    "stackEntry": None,
                },
            },
        ]
        normalized = normalize_merge_queue_response(
            merge_queue_response(
                total_count=5,
                has_next_page=True,
                nodes=nodes,
            )
        )["merge_queue"]
        self.assertEqual(normalized["total_count"], 5)
        self.assertTrue(normalized["truncated"])
        self.assertEqual(
            normalized["entries"][0]["stack_id"], "stack-a"
        )
        self.assertEqual(
            normalized["entries"][2]["stack_id"], "stack-b"
        )
        self.assertIsNone(normalized["entries"][3]["stack_id"])


    def test_queue_branch_resolution_uses_trunk_not_immediate_stack_base(self):
        stack = {"stack_status": "stack_member", "stack_base_ref": "master"}
        self.assertEqual(
            resolve_queue_branch(
                merge_group_base_ref=None,
                stack=stack,
                pull_request={"base": {"ref": "pr-3715"}},
            ),
            {"queue_branch": "master", "queue_branch_source": "GraphQL stack base"},
        )
        self.assertEqual(
            resolve_queue_branch(
                merge_group_base_ref="master",
                stack={"stack_status": "unresolved"},
                pull_request=None,
            )["queue_branch_source"],
            "merge_group.base_ref",
        )
        self.assertEqual(
            resolve_queue_branch(
                merge_group_base_ref=None,
                stack={"stack_status": "not_a_stack"},
                pull_request={"base": {"ref": "master"}},
            )["queue_branch_source"],
            "REST PR base",
        )
        self.assertEqual(
            resolve_queue_branch(stack={"stack_status": "unresolved"})["queue_branch_source"],
            "unresolved",
        )

    def test_queue_ref_branch_is_an_independent_trunk_signal(self):
        self.assertEqual(
            queue_branch_from_queue_ref("gh-readonly-queue/master/pr-3716-abcde"),
            "master",
        )

    def test_queue_corroboration_fields_stay_separate_from_entry_status(self):
        result = normalize_merge_queue_response(merge_queue_response(entry=True))
        self.assertEqual(result["queue_entry_status"], "present")
        self.assertTrue(result["is_in_merge_queue"])
        absent = normalize_merge_queue_response(merge_queue_response(entry=False))
        self.assertEqual(absent["queue_entry_status"], "absent")
        self.assertFalse(absent["is_in_merge_queue"])
        conflicting = merge_queue_response(entry=False)
        conflicting["data"]["repository"]["pullRequest"]["isInMergeQueue"] = True
        conflict_result = normalize_merge_queue_response(conflicting)
        self.assertEqual(conflict_result["queue_entry_status"], "absent")
        self.assertTrue(conflict_result["is_in_merge_queue"])
        unavailable = normalize_merge_queue_response({"errors": [{"message": "preview"}]})
        self.assertIsNone(unavailable["is_in_merge_queue"])


    def test_rest_error_evidence_is_sanitized(self):
        error = normalize_api_error(
            "candidate PR", 403, "http_error", b'{"message":"forbidden","token":"secret"}',
            timestamp="2026-01-01T00:00:00Z",
        )
        self.assertEqual(error["message"], "forbidden")
        self.assertNotIn("secret", str(error))
        self.assertEqual(
            normalize_api_error("candidate stack", 500, "http_error", {"message": "oops"})[
                "message"
            ],
            "oops",
        )

    def test_candidate_entry_failure_preserves_repository_topology(self):
        api = FakeQueueAPI(
            candidate={"errors": [{"message": "candidate preview unavailable"}]},
            repository=merge_queue_response(),
        )
        warnings = []
        with tempfile.TemporaryDirectory() as temporary:
            observation = _collect_queue_observation(
                api, 101, "master", "merge_group.base_ref", Path(temporary), warnings
            )
        self.assertEqual(observation["queue_entry_status"], "present")
        self.assertEqual(
            observation["effective_candidate_queue_entry_source"],
            "repository.mergeQueue.entries",
        )
        self.assertEqual(observation["merge_queue"]["id"], "queue-1")
        self.assertIn(("candidate", 101), api.calls)
        self.assertIn(("repository", "master"), api.calls)
        self.assertTrue(any("candidate merge-queue entry" in warning["message"] for warning in warnings))

    def test_repository_queue_failure_preserves_candidate_entry(self):
        api = FakeQueueAPI(candidate=merge_queue_response(), repository={"errors": [{"message": "queue unavailable"}]})
        warnings = []
        with tempfile.TemporaryDirectory() as temporary:
            observation = _collect_queue_observation(
                api, 101, "master", "merge_group.base_ref", Path(temporary), warnings
            )
        self.assertEqual(observation["queue_entry_status"], "present")
        self.assertEqual(observation["entry"]["id"], "entry-101")
        self.assertIsNone(observation["merge_queue"])
        self.assertTrue(any("repository merge-queue topology" in warning["message"] for warning in warnings))

    def test_unresolved_candidate_still_collects_known_master_queue(self):
        repository = merge_queue_response()
        repository["data"]["repository"].pop("pullRequest")
        api = FakeQueueAPI(candidate=None, repository=repository)
        warnings = []
        with tempfile.TemporaryDirectory() as temporary:
            observation = _collect_queue_observation(
                api, None, "master", "merge_group.base_ref", Path(temporary), warnings
            )
        self.assertEqual(observation["merge_queue"]["id"], "queue-1")
        self.assertEqual(api.calls, [("repository", "master")])
        self.assertIsNone(observation["effective_candidate_queue_entry"])

    def test_actual_generated_graphql_documents_parse(self):
        api = QueryCaptureAPI()
        api.stack_graphql(101)
        api.candidate_queue_entry_graphql(101)
        api.repository_merge_queue_graphql("master")
        self.assertEqual(len(api.queries), 3)
        for query, _, _ in api.queries:
            assert_graphql_document(query)
        stack_query = api.queries[0][0]
        self.assertNotIn("}}", stack_query)
        self.assertIn("stackEntry", stack_query)
        self.assertIn("entries(first: 100)", stack_query)
        self.assertEqual(api.queries[2][1]["branch"], "master")
        for query, _, _ in api.queries:
            self.assertNotIn("configuration", query)
            self.assertNotIn("timelineItems", query)
        with self.assertRaises(AssertionError):
            assert_graphql_document(stack_query + "}")


class GitHistoryTests(unittest.TestCase):
    def test_negative_duration_is_unresolved(self):
        self.assertIsNone(
            duration_seconds("2026-01-01T00:01:00Z", "2026-01-01T00:00:00Z")
        )

    def test_fetch_requests_are_bounded_and_do_not_unshallow(self):
        args = bounded_git_fetch_args("a" * 40)
        self.assertEqual(
            args,
            ["fetch", "--no-tags", f"--depth={GIT_FETCH_DEPTH}", "origin", "a" * 40],
        )
        self.assertNotIn("--unshallow", args)

    def test_positive_ancestry_is_known_in_shallow_history(self):
        self.assertTrue(ancestry_from_exit_code(0, True))

    def test_negative_ancestry_is_unresolved_when_history_is_shallow_or_unknown(self):
        self.assertIsNone(ancestry_from_exit_code(1, True))
        self.assertIsNone(ancestry_from_exit_code(1, None))

    def test_negative_ancestry_is_known_when_history_is_complete(self):
        self.assertFalse(ancestry_from_exit_code(1, False))


class StackClassificationTests(unittest.TestCase):
    def test_partial_stack_data_is_retained_but_classification_is_unresolved(self):
        response = graphql_pr(
            stack={"id": "stack-node", "number": 42, "size": 2, "baseRefName": "master"},
            entry={"position": 2},
        )
        response["errors"] = [{"message": "optional stack field unavailable"}]
        result = classify_stack(response)
        self.assertEqual(result["stack_status"], "unresolved")
        self.assertEqual(result["graphql_status"], "partial")
        self.assertEqual(result["graphql_errors"][0]["message"], "optional stack field unavailable")

    def test_native_stack_member_position_one(self):
        response = graphql_pr(
            stack={
                "id": "stack-node",
                "number": 42,
                "size": 2,
                "baseRefName": "master",
                "entries": {
                    "nodes": [
                        {
                            "position": 1,
                            "pullRequest": {"baseRefOid": "base-sha"},
                        },
                        {"position": 2, "pullRequest": {"baseRefOid": "head-sha"}},
                    ]
                },
            },
            entry={"position": 1},
        )
        result = classify_stack(response)
        self.assertEqual(result["stack_status"], "stack_member")
        self.assertEqual(result["stack_position"], 1)
        self.assertEqual(result["stack_size"], 2)
        self.assertFalse(result["is_stack_head"])

    def test_native_stack_head(self):
        response = graphql_pr(
            stack={
                "id": "stack-node",
                "number": 42,
                "size": 2,
                "baseRefName": "master",
                "entries": {"nodes": []},
            },
            entry={"position": 2},
        )
        result = classify_stack(response)
        self.assertTrue(result["is_stack_member"])
        self.assertTrue(result["is_stack_head"])

    def test_successful_null_fields_mean_not_a_stack(self):
        result = classify_stack(graphql_pr())
        self.assertEqual(result["stack_status"], "not_a_stack")
        self.assertFalse(result["is_stack_member"])
        self.assertFalse(result["is_stack_head"])

    def test_failed_or_incomplete_api_is_unresolved(self):
        self.assertEqual(
            classify_stack({"errors": [{"message": "preview unavailable"}]})[
                "stack_status"
            ],
            "unresolved",
        )
        self.assertEqual(classify_stack({"data": None})["stack_status"], "unresolved")
        incomplete = graphql_pr(stack={"number": 42}, entry=None)
        self.assertEqual(classify_stack(incomplete)["stack_status"], "unresolved")



class TimingTests(unittest.TestCase):
    def setUp(self):
        self.names = (
            "Code checks",
            "Cucumber integration tests",
            "End-to-end integration tests",
        )

    @staticmethod
    def run_row(
        name, run_id, created, started, updated, status="completed", attempt=1
    ):
        return {
            "name": name,
            "id": run_id,
            "run_attempt": attempt,
            "status": status,
            "conclusion": "success" if status == "completed" else None,
            "created_at": created,
            "run_started_at": started,
            "updated_at": updated,
        }

    def test_pending_sibling_is_partial(self):
        runs = {
            self.names[0]: self.run_row(
                self.names[0], 1, "2026-01-01T00:00:00Z",
                "2026-01-01T00:01:00Z", "2026-01-01T00:03:00Z"
            ),
            self.names[1]: self.run_row(
                self.names[1], 2, "2026-01-01T00:00:30Z",
                "2026-01-01T00:02:00Z", "2026-01-01T00:02:30Z",
                status="in_progress"
            ),
            self.names[2]: None,
        }
        result = derive_timing_metrics(runs, {})
        self.assertEqual(result["timing_status"], "partial")
        self.assertIsNone(result["candidate_wall_time_seconds"])

    def test_all_siblings_complete_has_workflow_and_candidate_timings(self):
        runs = {
            self.names[0]: self.run_row(
                self.names[0], 1, "2026-01-01T00:00:00Z",
                "2026-01-01T00:01:00Z", "2026-01-01T00:03:00Z"
            ),
            self.names[1]: self.run_row(
                self.names[1], 2, "2026-01-01T00:00:30Z",
                "2026-01-01T00:02:00Z", "2026-01-01T00:04:00Z"
            ),
            self.names[2]: self.run_row(
                self.names[2], 3, "2026-01-01T00:00:15Z",
                "2026-01-01T00:01:15Z", "2026-01-01T00:05:00Z"
            ),
        }
        jobs = {
            (1, 1): [
                {
                    "name": "Lint",
                    "status": "completed",
                    "started_at": "2026-01-01T00:01:00Z",
                    "completed_at": "2026-01-01T00:02:00Z",
                    "labels": ["ubuntu-latest"],
                    "runner_group_name": "GitHub Actions",

                }
            ]
        }
        result = derive_timing_metrics(runs, jobs)
        self.assertEqual(result["timing_status"], "complete")
        self.assertEqual(result["candidate_wall_time_seconds"], 300.0)
        self.assertEqual(result["runner_time_seconds"]["github-hosted"], 60.0)
        self.assertNotIn("steps", result["job_rows"][0])

    def test_workflow_start_delay_name_and_job_start_latencies(self):
        runs = {
            name: self.run_row(
                name,
                index,
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:01:00Z",
                "2026-01-01T00:02:00Z",
            )
            for index, name in enumerate(self.names, start=1)
        }
        for run in runs.values():
            run["head_sha"] = "candidate-sha"
        jobs = {
            (1, 1): [
                {
                    "id": 10,
                    "name": "Check Rust lints",
                    "status": "completed",
                    "conclusion": "success",
                    "started_at": "2026-01-01T00:01:30Z",
                    "completed_at": "2026-01-01T00:02:00Z",
                    "runner_id": 77,
                    "runner_group_id": 88,
                    "runner_group_name": "GitHub Actions",
                    "runner_name": "GitHub Actions 42",
                    "labels": ["ubuntu-latest"],
                }
            ]
        }
        result = derive_timing_metrics(runs, jobs)
        self.assertEqual(
            result["workflow_rows"][0]["workflow_created_to_run_started_seconds"],
            60.0,
        )
        self.assertNotIn("queue_delay_seconds", result["workflow_rows"][0])
        self.assertEqual(
            result["job_rows"][0]["workflow_created_to_job_started_seconds"],
            90.0,
        )
        self.assertEqual(
            result["job_rows"][0]["workflow_run_started_to_job_started_seconds"],
            30.0,
        )

    def test_candidate_wall_time_includes_earlier_attempt_for_retried_workflow(self):
        latest = {
            self.names[0]: self.run_row(
                self.names[0], 10, "2026-01-01T00:00:00Z",
                "2026-01-01T00:00:30Z", "2026-01-01T00:03:00Z", attempt=2
            ),
            self.names[1]: self.run_row(
                self.names[1], 12, "2026-01-01T00:00:30Z",
                "2026-01-01T00:01:00Z", "2026-01-01T00:04:00Z"
            ),
            self.names[2]: self.run_row(
                self.names[2], 13, "2026-01-01T00:00:30Z",
                "2026-01-01T00:01:00Z", "2026-01-01T00:05:00Z"
            ),
        }
        earlier_attempt = self.run_row(
            self.names[0], 10, "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:15Z", "2026-01-01T00:02:00Z", attempt=1
        )
        result = derive_timing_metrics(latest, {}, [*latest.values(), earlier_attempt])
        self.assertEqual(result["candidate_wall_time_seconds"], 300.0)

    def test_all_same_run_id_attempts_contribute_cost_but_wall_time_is_elapsed_time(self):
        code1 = workflow_attempt_record(
            123, 1, "Code checks", "failure", "2026-10-01T00:00:00Z",
            "2026-10-01T00:01:30Z", "2026-10-01T00:00:00Z"
        )
        code2 = workflow_attempt_record(
            123, 2, "Code checks", "success", "2026-10-01T00:02:00Z",
            "2026-10-01T00:04:00Z", "2026-10-01T00:00:00Z"
        )
        cucumber = workflow_attempt_record(
            124, 1, "Cucumber integration tests", "success", "2026-10-01T00:01:00Z",
            "2026-10-01T00:02:00Z", "2026-10-01T00:00:30Z"
        )
        e2e = workflow_attempt_record(
            125, 1, "End-to-end integration tests", "success", "2026-10-01T00:01:00Z",
            "2026-10-01T00:02:30Z", "2026-10-01T00:00:15Z"
        )
        jobs = {
            (123, 1): [attempt_job("lint", "failure", "2026-10-01T00:00:00Z", "2026-10-01T00:01:30Z", "Code checks")],
            (123, 2): [attempt_job("lint", "success", "2026-10-01T00:02:00Z", "2026-10-01T00:04:00Z", "Code checks")],
            (124, 1): [attempt_job("cucumber", "success", "2026-10-01T00:01:00Z", "2026-10-01T00:01:00Z", "Cucumber integration tests")],
            (125, 1): [attempt_job("e2e", "success", "2026-10-01T00:01:00Z", "2026-10-01T00:01:00Z", "End-to-end integration tests")],
        }
        api = FakeAttemptAPI(
            {(123, 1): code1, (123, 2): code2, (124, 1): cucumber, (125, 1): e2e},
            jobs,
        )
        warnings = []
        with tempfile.TemporaryDirectory() as temporary:
            attempts, jobs_by_run = _collect_workflow_attempts(
                api,
                [code2, cucumber, e2e],
                code2,
                Path(temporary),
                warnings,
            )
        timing = derive_timing_metrics(_latest_runs_by_name(attempts), jobs_by_run, attempts)
        self.assertEqual([(row["id"], row["run_attempt"]) for row in attempts if row["id"] == 123], [(123, 1), (123, 2)])
        self.assertEqual(api.attempt_calls.count((123, 1)), 1)
        self.assertEqual(api.attempt_calls.count((123, 2)), 1)
        code_row = next(row for row in timing["workflow_rows"] if row["workflow_name"] == "Code checks")
        self.assertEqual(code_row["run_id"], 123)
        self.assertEqual(code_row["run_attempt"], 2)
        self.assertEqual(code_row["conclusion"], "success")
        self.assertEqual(timing["runner_time_seconds"]["self-hosted"], 210.0)
        self.assertEqual(timing["timing_status"], "complete")
        self.assertEqual(timing["runner_time_status"], "complete")
        self.assertEqual(timing["runner_time_missing_attempts"], [])
        self.assertEqual(timing["candidate_wall_time_seconds"], 240.0)
        self.assertEqual({row["run_attempt"] for row in timing["job_rows"] if row["run_id"] == 123}, {1, 2})

    def test_single_attempt_is_not_collected_twice_from_listing_and_source(self):
        only = workflow_attempt_record(
            321, 1, "Code checks", "success", "2026-10-01T00:00:00Z",
            "2026-10-01T00:00:45Z", "2026-10-01T00:00:00Z"
        )
        job = attempt_job("lint", "success", "2026-10-01T00:00:00Z", "2026-10-01T00:00:45Z", "Code checks")
        api = FakeAttemptAPI({(321, 1): only}, {(321, 1): [job]})
        with tempfile.TemporaryDirectory() as temporary:
            attempts, jobs = _collect_workflow_attempts(
                api,
                [only],
                only,
                Path(temporary),
                [],
            )
        timing = derive_timing_metrics(_latest_runs_by_name(attempts), jobs, attempts)
        self.assertEqual(len(attempts), 1)
        self.assertEqual(api.attempt_calls, [(321, 1)])
        self.assertEqual(api.job_calls, [(321, 1)])
        self.assertEqual(timing["runner_time_seconds"]["self-hosted"], 45.0)

    def test_missing_historical_attempt_warns_and_keeps_latest_attempt(self):
        latest = workflow_attempt_record(
            456, 2, "Code checks", "success", "2026-10-01T00:02:00Z",
            "2026-10-01T00:04:00Z", "2026-10-01T00:00:00Z"
        )
        cucumber = workflow_attempt_record(
            457, 1, "Cucumber integration tests", "success",
            "2026-10-01T00:01:00Z", "2026-10-01T00:03:00Z",
            "2026-10-01T00:00:00Z"
        )
        e2e = workflow_attempt_record(
            458, 1, "End-to-end integration tests", "success",
            "2026-10-01T00:01:00Z", "2026-10-01T00:03:00Z",
            "2026-10-01T00:00:00Z"
        )
        job = attempt_job("lint", "success", "2026-10-01T00:02:00Z", "2026-10-01T00:04:00Z", "Code checks")
        cucumber_job = attempt_job(
            "cucumber", "success", "2026-10-01T00:01:00Z",
            "2026-10-01T00:03:00Z", "Cucumber integration tests"
        )
        e2e_job = attempt_job(
            "e2e", "success", "2026-10-01T00:01:00Z",
            "2026-10-01T00:03:00Z", "End-to-end integration tests"
        )
        api = FakeAttemptAPI(
            {(456, 1): None, (456, 2): latest, (457, 1): cucumber, (458, 1): e2e},
            {(456, 2): [job], (457, 1): [cucumber_job], (458, 1): [e2e_job]},
        )
        warnings = []
        with tempfile.TemporaryDirectory() as temporary:
            attempts, jobs = _collect_workflow_attempts(
                api, [latest, cucumber, e2e], latest, Path(temporary), warnings
            )
        self.assertEqual(len(attempts), 4)
        historical = next(
            row for row in attempts
            if row["id"] == 456 and row["run_attempt"] == 1
        )
        self.assertEqual(historical["status"], "unavailable")
        self.assertFalse(historical["attempt_metadata_available"])
        self.assertTrue(any(w["category"] == "historical_workflow_attempt_unavailable" for w in warnings))
        self.assertTrue(any(w["category"] == "workflow_attempt_jobs_unavailable" for w in warnings))
        timing = derive_timing_metrics(_latest_runs_by_name(attempts), jobs, attempts)
        self.assertEqual(timing["timing_status"], "complete")
        self.assertEqual(timing["runner_time_status"], "partial")
        self.assertEqual(
            timing["runner_time_missing_attempts"],
            [
                {
                    "run_id": 456,
                    "run_attempt": 1,
                    "workflow_name": "Code checks",
                    "reason": "jobs_unavailable",
                    "attempt_metadata_available": False,
                }
            ],
        )
        self.assertEqual(timing["workflow_rows"][0]["run_attempt"], 2)
        self.assertEqual(timing["runner_time_seconds"]["self-hosted"], 360.0)
        with tempfile.TemporaryDirectory() as temporary:
            summary_path = Path(temporary) / "summary.md"
            _write_timing_summary(
                {
                    "timing": timing,
                    "identity": {
                        "candidate_pr": 101,
                        "candidate_pr_evidence": [],
                        "stack": {"stack_status": "not_a_stack"},
                    },
                    "observed_sibling_workflows": attempts,
                    "checks": {"metrics": {}},
                },
                summary_path,
            )
            summary_text = summary_path.read_text(encoding="utf-8")
        self.assertIn("**Runner-cost accounting:** partial", summary_text)
        self.assertIn("Code checks run 456 / attempt 1", summary_text)

    def test_attempt_metadata_available_but_jobs_unavailable_marks_cost_partial(self):
        first = workflow_attempt_record(
            512, 1, "Code checks", "failure", "2026-10-01T00:00:00Z",
            "2026-10-01T00:01:30Z", "2026-10-01T00:00:00Z"
        )
        latest = workflow_attempt_record(
            512, 2, "Code checks", "success", "2026-10-01T00:02:00Z",
            "2026-10-01T00:04:00Z", "2026-10-01T00:00:00Z"
        )
        latest_job = attempt_job(
            "lint", "success", "2026-10-01T00:02:00Z",
            "2026-10-01T00:04:00Z", "Code checks"
        )
        api = FakeAttemptAPI(
            {(512, 1): first, (512, 2): latest}, {(512, 2): [latest_job]}
        )
        with tempfile.TemporaryDirectory() as temporary:
            attempts, jobs = _collect_workflow_attempts(
                api, [latest], latest, Path(temporary), []
            )
        timing = derive_timing_metrics(_latest_runs_by_name(attempts), jobs, attempts)
        missing = timing["runner_time_missing_attempts"]
        self.assertEqual(timing["runner_time_status"], "partial")
        self.assertEqual(missing[0]["run_attempt"], 1)
        self.assertTrue(missing[0]["attempt_metadata_available"])
        self.assertEqual(missing[0]["reason"], "jobs_unavailable")
        self.assertEqual(timing["runner_time_seconds"]["self-hosted"], 120.0)

    def test_attempt_metadata_unavailable_with_complete_jobs_keeps_cost_complete(self):
        latest = workflow_attempt_record(
            513, 2, "Code checks", "success", "2026-10-01T00:02:00Z",
            "2026-10-01T00:04:00Z", "2026-10-01T00:00:00Z"
        )
        historical_job = attempt_job(
            "lint", "failure", "2026-10-01T00:00:00Z",
            "2026-10-01T00:01:30Z", "Code checks"
        )
        latest_job = attempt_job(
            "lint", "success", "2026-10-01T00:02:00Z",
            "2026-10-01T00:04:00Z", "Code checks"
        )
        api = FakeAttemptAPI(
            {(513, 1): None, (513, 2): latest},
            {(513, 1): [historical_job], (513, 2): [latest_job]},
        )
        with tempfile.TemporaryDirectory() as temporary:
            attempts, jobs = _collect_workflow_attempts(
                api, [latest], latest, Path(temporary), []
            )
        timing = derive_timing_metrics(_latest_runs_by_name(attempts), jobs, attempts)
        historical = next(run for run in attempts if run["run_attempt"] == 1)
        self.assertFalse(historical["attempt_metadata_available"])
        self.assertTrue(historical["jobs_complete"])
        self.assertEqual(timing["runner_time_status"], "complete")
        self.assertEqual(timing["runner_time_missing_attempts"], [])
        self.assertEqual(timing["runner_time_seconds"]["self-hosted"], 210.0)

    def test_cancelled_retry_still_counts_work_performed_by_its_jobs(self):
        first = workflow_attempt_record(
            654, 1, "Code checks", "failure", "2026-10-01T00:00:00Z",
            "2026-10-01T00:01:30Z", "2026-10-01T00:00:00Z"
        )
        cancelled = workflow_attempt_record(
            654, 2, "Code checks", "cancelled", "2026-10-01T00:02:00Z",
            "2026-10-01T00:02:30Z", "2026-10-01T00:00:00Z"
        )
        api = FakeAttemptAPI(
            {(654, 1): first, (654, 2): cancelled},
            {
                (654, 1): [attempt_job("lint", "failure", "2026-10-01T00:00:00Z", "2026-10-01T00:01:30Z", "Code checks")],
                (654, 2): [attempt_job("lint", "cancelled", "2026-10-01T00:02:00Z", "2026-10-01T00:02:30Z", "Code checks")],
            },
        )
        with tempfile.TemporaryDirectory() as temporary:
            attempts, jobs = _collect_workflow_attempts(
                api, [cancelled], cancelled, Path(temporary), []
            )
        timing = derive_timing_metrics(_latest_runs_by_name(attempts), jobs, attempts)
        self.assertEqual(timing["workflow_rows"][0]["conclusion"], "cancelled")
        self.assertEqual(timing["runner_time_seconds"]["self-hosted"], 120.0)
        self.assertEqual(timing["runner_time_status"], "complete")

    def test_skipped_jobs_do_not_make_complete_job_listings_partial(self):
        runs = {
            name: workflow_attempt_record(
                index, 1, name, "success", "2026-10-01T00:00:00Z",
                "2026-10-01T00:01:00Z", "2026-10-01T00:00:00Z"
            )
            for index, name in enumerate(self.names, start=601)
        }
        jobs = {
            (runs[self.names[0]]["id"], 1): [
                attempt_job(
                    "conditional", "skipped", "2026-10-01T00:00:00Z",
                    "2026-10-01T00:00:00Z", self.names[0]
                )
            ],
            (runs[self.names[1]]["id"], 1): [],
            (runs[self.names[2]]["id"], 1): [],
        }
        timing = derive_timing_metrics(runs, jobs)
        self.assertEqual(timing["timing_status"], "complete")
        self.assertEqual(timing["runner_time_status"], "complete")
        self.assertEqual(timing["runner_time_seconds"]["self-hosted"], 0.0)

    def test_multiple_workflows_keep_their_own_attempts(self):
        rows = [
            workflow_attempt_record(run_id, attempt, name, conclusion, start, end, created)
            for run_id, attempt, name, conclusion, start, end, created in (
                (700, 2, "Code checks", "success", "2026-10-01T00:02:00Z", "2026-10-01T00:03:00Z", "2026-10-01T00:00:00Z"),
                (701, 2, "Cucumber integration tests", "success", "2026-10-01T00:02:00Z", "2026-10-01T00:04:00Z", "2026-10-01T00:00:00Z"),
                (702, 1, "End-to-end integration tests", "success", "2026-10-01T00:01:00Z", "2026-10-01T00:02:00Z", "2026-10-01T00:00:00Z"),
            )
        ]
        details = {}
        jobs = {}
        for latest in rows:
            run_id = latest["id"]
            name = latest["name"]
            count = latest["run_attempt"]
            for number in range(1, count + 1):
                conclusion = "failure" if number == 1 and count > 1 else "success"
                start = f"2026-10-01T00:0{number}:00Z"
                end = f"2026-10-01T00:0{number}:30Z"
                details[(run_id, number)] = workflow_attempt_record(
                    run_id, number, name, conclusion, start, end, latest["created_at"]
                )
                jobs[(run_id, number)] = [
                    attempt_job("job", conclusion, start, end, name)
                ]
        api = FakeAttemptAPI(details, jobs)
        with tempfile.TemporaryDirectory() as temporary:
            attempts, collected_jobs = _collect_workflow_attempts(
                api, rows, rows[0], Path(temporary), []
            )
        self.assertEqual({run["id"] for run in attempts}, {700, 701, 702})
        self.assertEqual(
            {(run["id"], run["run_attempt"]) for run in attempts},
            {(700, 1), (700, 2), (701, 1), (701, 2), (702, 1)},
        )
        self.assertEqual(len(collected_jobs), 5)


    def test_skipped_job_without_runner_details_does_not_warn(self):
        self.assertFalse(
            runner_metadata_missing_unexpectedly(
                {"status": "completed", "conclusion": "skipped"}
            )
        )

    def test_cancelled_job_that_never_started_does_not_warn(self):
        self.assertFalse(
            runner_metadata_missing_unexpectedly(
                {"status": "completed", "conclusion": "cancelled", "started_at": None}
            )
        )

    def test_started_job_without_runner_details_warns(self):
        self.assertTrue(
            runner_metadata_missing_unexpectedly(
                {
                    "status": "completed",
                    "conclusion": "success",
                    "started_at": "2026-01-01T00:00:00Z",
                }
            )
        )

    def test_completed_ran_job_without_start_timestamp_still_warns(self):
        self.assertTrue(
            runner_metadata_missing_unexpectedly(
                {"status": "completed", "conclusion": "success"}
            )
        )


def assert_graphql_document(query):
    """Parse the small GraphQL syntax subset emitted by retained read queries.

    This checks the generated document, including arguments, variable types,
    and selections. It is a local syntax check, not server schema validation.
    """
    query = query.replace(",", " ")
    tokens = re.findall(r"[A-Za-z_][A-Za-z_0-9]*|[0-9]+|[$!():{}]", query)
    if "".join(tokens) != re.sub(r"\s+", "", query):
        raise AssertionError("Unsupported or invalid GraphQL token")
    index = 0

    def take(expected=None):
        nonlocal index
        if index >= len(tokens):
            raise AssertionError("Unexpected end of GraphQL document")
        token = tokens[index]
        index += 1
        if expected is not None and token != expected:
            raise AssertionError(f"Expected {expected}, got {token}")
        return token

    def name():
        token = take()
        if not re.fullmatch(r"[A-Za-z_][A-Za-z_0-9]*", token):
            raise AssertionError(f"Expected name, got {token}")

    def arguments(definitions=False):
        take("(")
        while tokens[index] != ")":
            if definitions:
                take("$")
            name()
            take(":")
            if not definitions and tokens[index] == "$":
                take("$")
                name()
            else:
                token = take()
                if not re.fullmatch(r"[A-Za-z_][A-Za-z_0-9]*|[0-9]+", token):
                    raise AssertionError("Invalid argument/type")
            if definitions and tokens[index] == "!":
                take("!")
        take(")")

    def selection():
        take("{")
        name()
        while True:
            if tokens[index] == "(":
                arguments()
            if tokens[index] == "{":
                selection()
            if tokens[index] == "}":
                take("}")
                return
            name()

    # GraphQL permits commas as insignificant separators.
    take("query")
    arguments(definitions=True)
    selection()
    if index != len(tokens):
        raise AssertionError("Trailing GraphQL tokens")


class FakeIdentityAPI:
    """Return only a specified candidate lookup; record which PR is queried."""

    def __init__(self, number, stack_response):
        self.number = number
        self.stack_response = stack_response
        self.stack_calls = []

    def associated_pulls(self, sha):
        return [{"number": self.number}, {"number": 901}]

    def pull_request(self, number):
        return {"number": number, "base": {"ref": "master"}}

    def stack_graphql(self, number):
        self.stack_calls.append(number)
        return self.stack_response


def own_stack_response(number, position, *, size=3):
    """Model bounded native stack S; member heads are real-shaped object IDs."""
    stack = {
        "id": "S", "number": 3717, "size": size, "baseRefName": "master",
        "entries": {"totalCount": size, "nodes": [
            {"position": index, "pullRequest": {
                "number": 3713 + index, "headRefOid": str(index) * 40,
                "baseRefOid": str(index - 1) * 40,
            }} for index in range(1, size + 1)
        ]},
    }
    response = graphql_pr(stack, {"position": position})
    response["data"]["repository"]["pullRequest"]["number"] = number
    return response


class ObjectiveIsolationTests(unittest.TestCase):
    def observe(self, number, response, queue_nodes):
        api = FakeIdentityAPI(number, response)
        response["data"]["repository"]["pullRequest"]["number"] = number
        with tempfile.TemporaryDirectory() as temporary:
            identity, _ = _collect_identity(
                api, "a" * 40, [("merge_group.head_ref", f"gh-readonly-queue/master/pr-{number}-abcde")],
                Path(temporary), [],
            )
            queue = _collect_queue_observation(
                FakeQueueAPI(repository=merge_queue_response(nodes=queue_nodes, total_count=len(queue_nodes))),
                number, "master", "merge_group.base_ref", Path(temporary), [],
            )
        self.assertEqual(api.stack_calls, [number])
        self.assertEqual(len(queue["merge_queue"]["entries"]), len(queue_nodes))
        return identity

    @staticmethod
    def queue_entry(number, stack_id=None, position=None):
        return {"id": f"entry-{number}", "pullRequest": {
            "number": number,
            "stack": {"id": stack_id, "number": 1, "size": 3} if stack_id else None,
            "stackEntry": {"position": position} if stack_id else None,
        }}

    def test_ordinary_alone_and_with_other_stack_remains_not_a_stack(self):
        for nodes in ([self.queue_entry(101)], [self.queue_entry(101), self.queue_entry(901, "T", 1)]):
            identity = self.observe(101, graphql_pr(), nodes)
            self.assertEqual(identity["stack"]["stack_status"], "not_a_stack")
            self.assertIsNone(expected_stack_members(identity["stack"], 101))

    def test_candidate_own_stack_isolated_from_ordinary_and_sister_stack(self):
        nodes = [self.queue_entry(101), self.queue_entry(3715, "S", 2), self.queue_entry(901, "T", 3)]
        identity = self.observe(3715, own_stack_response(3715, 2), nodes)
        stack = identity["stack"]
        self.assertEqual((stack["stack_id"], stack["stack_position"], stack["stack_size"]), ("S", 2, 3))
        self.assertFalse(stack["is_stack_head"])
        members = expected_stack_members(stack, 3715)
        self.assertEqual([row["pr_number"] for row in members], [3714, 3715])
        self.assertNotIn(901, [row["pr_number"] for row in members])

    def test_stack_head_remains_head_with_other_stacks_queued(self):
        identity = self.observe(3716, own_stack_response(3716, 3), [self.queue_entry(3716, "S", 3), self.queue_entry(901, "T", 1)])
        self.assertTrue(identity["stack"]["is_stack_head"])
        self.assertEqual(len(expected_stack_members(identity["stack"], 3716)), 3)

    def test_missing_duplicate_or_wrong_candidate_member_is_unresolved(self):
        for change in ("missing", "duplicate", "wrong_candidate"):
            response = own_stack_response(3715, 2)
            nodes = response["data"]["repository"]["pullRequest"]["stack"]["entries"]["nodes"]
            if change == "missing":
                nodes.pop(0)
            elif change == "duplicate":
                nodes.append(nodes[0])
            else:
                nodes[1]["pullRequest"]["number"] = 901
            self.assertIsNone(expected_stack_members(classify_stack(response), 3715))

    def test_partial_and_unavailable_stack_data_never_supply_positive_membership(self):
        partial = own_stack_response(3715, 2)
        partial["errors"] = [{"message": "preview failed", "path": ["repository", "pullRequest", "stack"]}]
        for response in (partial, {"errors": [{"message": "unavailable"}]}, {"data": None}):
            stack = classify_stack(response)
            self.assertEqual(stack["stack_status"], "unresolved")
            self.assertIsNone(stack["is_stack_member"])
            self.assertIsNone(expected_stack_members(stack, 3715))

    def test_targeted_git_fetch_checks_only_expected_prefix_of_own_stack(self):
        stack = classify_stack(own_stack_response(3715, 2))
        with tempfile.TemporaryDirectory() as temporary, patch(
            "scripts.ci.merge_queue_telemetry._git", return_value="true\n"
        ) as git, patch("scripts.ci.merge_queue_telemetry._git_is_ancestor", return_value=True) as ancestor:
            proof = _stack_composition(Path(temporary), "a" * 40, 3715, stack, Path(temporary), [])
        self.assertEqual(proof["status"], "proven")
        self.assertEqual(proof["fetch_objects"], ["a" * 40, "1" * 40, "2" * 40])
        self.assertEqual(ancestor.call_count, 2)
        self.assertIn(f"--depth={GIT_FETCH_DEPTH}", git.call_args_list[0].args[1])
        for classification in (classify_stack(graphql_pr()), classify_stack(None)):
            with patch("scripts.ci.merge_queue_telemetry._git") as git:
                _stack_composition(Path("."), "a" * 40, 101, classification, Path("."), [])
                git.assert_not_called()

    def test_snapshot_preserves_raw_base_ref_and_normalizes_only_queue_branch(self):
        class SnapshotAPI(FakeIdentityAPI, FakeQueueAPI):
            repo_name = "owner/repo"
            api_errors = []

            def __init__(self):
                FakeIdentityAPI.__init__(self, 101, graphql_pr())
                FakeQueueAPI.__init__(self, repository=merge_queue_response())

            def rest(self, path, **kwargs):
                return {"workflow_runs": []}

        import json
        api = SnapshotAPI()
        event = {"action": "checks_requested", "merge_group": {
            "head_sha": "a" * 40, "base_sha": "b" * 40,
            "base_ref": "refs/heads/master",
            "head_ref": "refs/heads/gh-readonly-queue/master/pr-101-abcde",
        }}
        with tempfile.TemporaryDirectory() as temporary, patch(
            "scripts.ci.merge_queue_telemetry.GitHubAPI", return_value=api
        ), patch("scripts.ci.merge_queue_telemetry.safe_environment", return_value={}):
            root = Path(temporary)
            source = root / "source.json"
            source.write_text(json.dumps(event), encoding="utf-8")
            result = _snapshot(source, root, root / "telemetry", root / "summary.md")
            self.assertEqual(source.read_bytes(), (root / "telemetry/event.json").read_bytes())
            self.assertEqual(result["merge_group"]["base_ref"], "refs/heads/master")
            self.assertEqual(result["queue"]["queue_branch"], "master")
            self.assertEqual(result["composition"]["status"], "not_applicable")
            self.assertIn(("repository", "master"), api.calls)
            self.assertEqual(api.stack_calls, [101])
            self.assertFalse((root / "telemetry/api/checks").exists())
            self.assertFalse((root / "telemetry/api/timeline").exists())

    def test_queue_timing_uses_only_this_sha_target_workflows_and_current_admission(self):
        runs = [workflow_attempt_record(index, 1, name, "success", "2026-01-01T00:01:00Z", "2026-01-01T00:03:00Z", "2026-01-01T00:00:30Z") for index, name in enumerate(TARGET_WORKFLOWS)]
        runs += [dict(runs[0], head_sha="another-sha", created_at="2025-01-01T00:00:00Z"), dict(runs[0], name="unrelated", updated_at="2027-01-01T00:00:00Z")]
        timing = derive_queue_timing("2026-01-01T00:04:00Z", {"enqueued_at": "2026-01-01T00:00:00Z"}, runs, candidate_sha="candidate-sha")
        self.assertEqual(timing["queue_age_at_snapshot_seconds"], 240)
        self.assertEqual(timing["queue_to_first_target_workflow_created_seconds"], 30)
        self.assertEqual(timing["queue_to_first_target_workflow_started_seconds"], 60)
        self.assertEqual(timing["queue_to_target_workflows_complete_seconds"], 180)
        # A newer queue admission cannot be paired with this candidate's older dispatch.
        timing = derive_queue_timing("2026-01-01T00:04:00Z", {"enqueued_at": "2026-01-01T00:02:00Z"}, runs, candidate_sha="candidate-sha")
        self.assertIsNone(timing["queue_to_target_workflows_complete_seconds"])


class WorkflowStructureTests(unittest.TestCase):
    def test_telemetry_workflow_is_read_only_and_hosted_only(self):
        workflow = (
            Path(__file__).parents[3]
            / ".github"
            / "workflows"
            / "merge-queue-telemetry.yml"
        ).read_text(encoding="utf-8")
        self.assertEqual(workflow.count("runs-on: ubuntu-latest"), 2)
        self.assertNotIn("runs-on: self-hosted", workflow)
        self.assertNotIn("runs-on: ift-", workflow.casefold())
        self.assertNotIn("checks: read", workflow)
        self.assertNotIn("statuses: read", workflow)
        self.assertIn("branches:\n      - 'gh-readonly-queue/**'", workflow)
        self.assertNotIn("pull_request:", workflow)
        self.assertEqual(workflow.count("ref: master"), 2)
        self.assertNotIn(": write", workflow)
        self.assertEqual(workflow.count("actions: read"), 2)
        self.assertEqual(workflow.count("contents: read"), 2)
        self.assertEqual(workflow.count("pull-requests: read"), 2)
        self.assertEqual(workflow.count("retention-days: 30"), 2)
        self.assertNotIn("concurrency:", workflow)


if __name__ == "__main__":
    unittest.main()
