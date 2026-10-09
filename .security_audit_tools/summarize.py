#!/usr/bin/env python3
"""Summarize security-audit events from node logs without inferring delivery."""

from __future__ import annotations

import argparse
import csv
import datetime as dt
import json
import math
import re
import statistics
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any


METRICS = (
    "slot_to_ordinary_submission_ms",
    "ordinary_proposal_ms",
    "sibling_construction_ms",
    "sibling_blend_enqueue_ms",
    "additional_processing_ms",
    "total_leadership_processing_ms",
)
TIMING_EVENT = "security_audit_leadership_timing"
SIBLING_EVENT = "security_audit_sibling_submitted"
RECEIVER_STAGES = {
    "security_audit_blend_payload_decapsulated": "blend_received",
    "security_audit_chain_proposal_received": "chain_proposal_received",
    "security_audit_proposal_reconstructed": "reconstructed",
    "security_audit_proposal_reconstruction_failed": "reconstruction_failed",
    "security_audit_proposal_accepted": "accepted",
    "security_audit_proposal_rejected": "rejected",
    "security_audit_proposal_apply_failed": "apply_failed",
    "security_audit_proposal_not_processed": "not_processed",
    "security_audit_block_storage_observed": "storage_observed",
}
REQUIRED_TIMING_FIELDS = (
    "parent_block_id",
    "slot",
    "configured_siblings",
    "constructed_siblings",
    "failed_siblings",
    "relay_enqueued_siblings",
    *METRICS,
)
REQUIRED_IDENTITY_FIELDS = (
    "ordinary_block_id",
    "sibling_block_id",
    "parent_block_id",
    "slot",
    "sibling_index",
    "relay_enqueued",
)
FIELD_RE = re.compile(
    r'(?P<key>[A-Za-z_][A-Za-z0-9_.-]*)=(?:"(?P<quoted>(?:\\.|[^"\\])*)"|(?P<bare>[^\s]+))'
)
TIMESTAMP_RE = re.compile(r"^\[?(?P<stamp>\d{4}-\d\d-\d\d[T ][^\s\]]+)")


def parse_timestamp(value: Any) -> dt.datetime | None:
    if isinstance(value, (int, float)):
        return dt.datetime.fromtimestamp(value, tz=dt.timezone.utc)
    if not isinstance(value, str):
        return None
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=dt.timezone.utc)
    return parsed.astimezone(dt.timezone.utc)


def parse_fields(line: str) -> dict[str, Any]:
    stripped = line.lstrip()
    if stripped.startswith("{"):
        try:
            record = json.loads(stripped)
        except json.JSONDecodeError:
            return {}
        if not isinstance(record, dict):
            return {}
        fields = record.get("fields")
        if isinstance(fields, dict):
            return {**record, **fields}
        return record

    fields: dict[str, Any] = {}
    for match in FIELD_RE.finditer(line):
        value = match.group("quoted")
        if value is not None:
            try:
                value = json.loads(f'"{value}"')
            except json.JSONDecodeError:
                pass
        else:
            value = match.group("bare")
        fields[match.group("key")] = value
    return fields


def parse_log(log_name: str, path: Path) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    events: list[dict[str, Any]] = []
    anomalies: list[dict[str, Any]] = []
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError as error:
        return [], [{"type": "log_read_failed", "node": log_name, "path": str(path), "detail": str(error)}]

    for line_number, line in enumerate(lines, start=1):
        if line.lstrip().startswith("{"):
            try:
                json.loads(line.lstrip())
            except json.JSONDecodeError as error:
                anomalies.append({
                    "type": "malformed_json_log_line",
                    "node": log_name,
                    "path": str(path),
                    "line": line_number,
                    "detail": str(error),
                })
                continue
        fields = parse_fields(line)
        event_name = fields.get("event")
        if not isinstance(event_name, str) or not event_name.startswith("security_audit_"):
            continue

        stamp = parse_timestamp(fields.get("timestamp") or fields.get("time") or fields.get("ts"))
        if stamp is None:
            match = TIMESTAMP_RE.match(line)
            if match:
                stamp = parse_timestamp(match.group("stamp"))
        event = {
            "node": log_name,
            "path": str(path),
            "line": line_number,
            "timestamp": stamp.isoformat().replace("+00:00", "Z") if stamp else None,
            "_timestamp": stamp,
            **fields,
        }
        if stamp is None:
            # A missing timestamp weakens ordering evidence but does not make
            # the recorded protocol outcome false.
            anomalies.append(
                {"type": "event_missing_timestamp", "node": log_name, "path": str(path), "line": line_number, "event": event_name}
            )
        events.append(event)
    return events, anomalies


def parse_window(start: str, end: str, index: int) -> dict[str, Any]:
    start_time = parse_timestamp(start)
    end_time = parse_timestamp(end)
    if start_time is None or end_time is None or end_time <= start_time:
        raise ValueError(f"invalid measurement window: {start!r} .. {end!r}")
    return {
        "name": f"window-{index}",
        "start": start_time,
        "end": end_time,
    }


def event_in_window(event: dict[str, Any], window: dict[str, Any] | None) -> bool:
    if window is None:
        return True
    stamp = event.get("_timestamp")
    return isinstance(stamp, dt.datetime) and window["start"] <= stamp < window["end"]


def as_int(fields: dict[str, Any], key: str, anomalies: list[dict[str, Any]], event: dict[str, Any]) -> int | None:
    try:
        return int(fields[key])
    except (KeyError, TypeError, ValueError):
        anomalies.append(
            {"type": "invalid_integer_field", "node": event["node"], "line": event["line"], "event": event.get("event"), "field": key, "value": fields.get(key)}
        )
        return None


def truthy(value: Any) -> bool:
    return value is True or str(value).lower() in {"true", "1", "yes"}


def summarize_values(values: list[float]) -> dict[str, float | int]:
    return {
        "samples": len(values),
        "min": min(values),
        "median": statistics.median(values),
        "max": max(values),
    }


REQUIRED_RECEIVER_PATH = (
    "blend_received",
    "chain_proposal_received",
    "reconstructed",
    "accepted",
)


def _event_precedes(first: dict[str, Any], second: dict[str, Any]) -> bool:
    first_time = first.get("_timestamp")
    second_time = second.get("_timestamp")
    if not isinstance(first_time, dt.datetime) or not isinstance(second_time, dt.datetime):
        return False
    if first_time < second_time:
        return True
    return (
        first_time == second_time
        and first.get("path") == second.get("path")
        and first.get("line", 0) < second.get("line", 0)
    )


def _ordered_receiver_path(events: list[dict[str, Any]]) -> bool:
    by_stage = {
        stage: [event for event in events if event.get("stage") == stage]
        for stage in REQUIRED_RECEIVER_PATH
    }
    return any(
        _event_precedes(blend, chain)
        and _event_precedes(chain, reconstructed)
        and _event_precedes(reconstructed, accepted)
        for blend in by_stage["blend_received"]
        for chain in by_stage["chain_proposal_received"]
        for reconstructed in by_stage["reconstructed"]
        for accepted in by_stage["accepted"]
    )


def _event_sort_key(event: dict[str, Any]) -> tuple[Any, str, int]:
    timestamp = event.get("_timestamp")
    return (
        timestamp if isinstance(timestamp, dt.datetime) else dt.datetime.min.replace(tzinfo=dt.timezone.utc),
        str(event.get("path", "")),
        int(event.get("line", 0)),
    )


def _public_event(event: dict[str, Any]) -> dict[str, Any]:
    return {key: value for key, value in event.items() if key != "_timestamp"}


def _receiver_status(events: list[dict[str, Any]]) -> dict[str, Any]:
    stage_names = {event["stage"] for event in events}
    reconstruction_status = (
        "reconstructed"
        if "reconstructed" in stage_names
        else "failed"
        if "reconstruction_failed" in stage_names
        else "not_observed"
    )
    application_status = (
        "accepted"
        if "accepted" in stage_names
        else "rejected"
        if "rejected" in stage_names
        else "apply_failed"
        if "apply_failed" in stage_names
        else "not_processed"
        if "not_processed" in stage_names
        else "not_observed"
    )
    storage_events = [event for event in events if event.get("stage") == "storage_observed"]
    reconstruction_failures = [event for event in events if event.get("stage") == "reconstruction_failed"]
    application_failures = [
        event for event in events if event.get("stage") in {"rejected", "apply_failed"}
    ]
    def failure_reasons(records: list[dict[str, Any]]) -> list[dict[str, Any]]:
        return [
            {
                "stage": event.get("stage"),
                "reason": next(
                    (
                        event.get(field)
                        for field in ("reason", "error", "failure_reason", "error_reason")
                        if event.get(field) is not None
                    ),
                    None,
                ),
            }
            for event in records
        ]
    outcome = (
        "accepted"
        if "accepted" in stage_names
        else "reconstruction_failed"
        if reconstruction_failures
        else "proposal_rejected"
        if "rejected" in stage_names
        else "application_failed"
        if "apply_failed" in stage_names
        else "not_processed"
        if "not_processed" in stage_names
        else "receiver_observation_incomplete"
        if events
        else "receiver_observation_missing"
    )
    return {
        "blend_received": "blend_received" in stage_names,
        "blend_status": "received" if "blend_received" in stage_names else "not_observed",
        "chain_proposal_received": "chain_proposal_received" in stage_names,
        "reconstructed": "reconstructed" in stage_names,
        "reconstruction_failed": "reconstruction_failed" in stage_names,
        "reconstruction_status": reconstruction_status,
        "accepted": "accepted" in stage_names,
        "rejected": "rejected" in stage_names,
        "apply_failed": "apply_failed" in stage_names,
        "not_processed": "not_processed" in stage_names,
        "application_status": application_status,
        "outcome": outcome,
        "reconstruction_failure_reasons": failure_reasons(reconstruction_failures),
        "application_failure_reasons": failure_reasons(application_failures),
        "stored": bool(storage_events),
        "canonical_status": _canonical_status(storage_events),
        "ordered_blend_to_acceptance": _ordered_receiver_path(events),
        "evidence_events": [_public_event(event) for event in sorted(events, key=_event_sort_key)],
    }


def analyze(
    events: list[dict[str, Any]],
    windows: list[dict[str, Any]],
    expected_siblings: int | None,
    producer_node: str | None = None,
) -> dict[str, Any]:
    anomalies: list[dict[str, Any]] = []
    incomplete_observations: list[dict[str, Any]] = []
    filtered_events: list[tuple[dict[str, Any], str]] = []
    selected_windows: list[dict[str, Any] | None] = windows or [None]
    for window in selected_windows:
        window_name = window["name"] if window else "all"
        filtered_events.extend((event, window_name) for event in events if event_in_window(event, window))

    timing_groups: dict[tuple[str, str, str, str], list[dict[str, Any]]] = defaultdict(list)
    identity_groups: dict[tuple[str, str, str, str], list[dict[str, Any]]] = defaultdict(list)
    receiver_events: list[dict[str, Any]] = []
    all_timing: list[tuple[dict[str, Any], str]] = []

    for event, window_name in filtered_events:
        event_name = event.get("event")
        if event_name == TIMING_EVENT:
            missing = [field for field in REQUIRED_TIMING_FIELDS if field not in event]
            if missing:
                anomalies.append({"type": "incomplete_timing_event", "node": event["node"], "line": event["line"], "missing_fields": missing})
            parent = str(event.get("parent_block_id", ""))
            slot = str(event.get("slot", ""))
            if not parent or not slot:
                anomalies.append({"type": "timing_event_missing_correlation_key", "node": event["node"], "line": event["line"]})
                continue
            timing_groups[(window_name, event["node"], parent, slot)].append(event)
            all_timing.append((event, window_name))
        elif event_name == SIBLING_EVENT:
            missing = [field for field in REQUIRED_IDENTITY_FIELDS if field not in event]
            if missing:
                anomalies.append({"type": "incomplete_sibling_identity", "node": event["node"], "line": event["line"], "missing_fields": missing})
            parent = str(event.get("parent_block_id", ""))
            slot = str(event.get("slot", ""))
            ordinary = str(event.get("ordinary_block_id", ""))
            if not parent or not slot or not ordinary:
                anomalies.append({"type": "sibling_identity_missing_correlation_key", "node": event["node"], "line": event["line"]})
                continue
            identity_groups[(window_name, event["node"], parent, slot)].append(event)
        elif event_name in RECEIVER_STAGES:
            sibling_id = str(event.get("sibling_block_id") or event.get("block_id") or "")
            if sibling_id:
                receiver_events.append({**event, "stage": RECEIVER_STAGES[event_name], "window": window_name})
            else:
                anomalies.append({"type": "receiver_event_missing_block_id", "node": event["node"], "line": event["line"], "event": event_name})

    if not all_timing:
        anomalies.append({"type": "no_leadership_timing_events"})

    opportunities: list[dict[str, Any]] = []
    matched_identity_keys: set[tuple[str, str, str, str]] = set()
    seen_sibling_ids: set[str] = set()
    metric_values: dict[tuple[str, int, str], list[float]] = defaultdict(list)

    for key, rows in sorted(timing_groups.items()):
        window_name, producer, parent, slot = key
        identity_key = key
        identities = identity_groups.get(identity_key, [])
        if identities:
            matched_identity_keys.add(identity_key)
        if len(rows) > 1:
            anomalies.append({"type": "duplicate_timing_opportunity", "node": producer, "parent_block_id": parent, "slot": slot, "records": len(rows), "window": window_name})

        for row in rows:
            local_anomalies: list[dict[str, Any]] = []
            configured = as_int(row, "configured_siblings", local_anomalies, row)
            constructed = as_int(row, "constructed_siblings", local_anomalies, row)
            failed = as_int(row, "failed_siblings", local_anomalies, row)
            relay_count = as_int(row, "relay_enqueued_siblings", local_anomalies, row)
            anomalies.extend(local_anomalies)
            for field, value in (
                ("configured_siblings", configured),
                ("constructed_siblings", constructed),
                ("failed_siblings", failed),
                ("relay_enqueued_siblings", relay_count),
            ):
                if value is not None and value < 0:
                    anomalies.append({
                        "type": "invalid_sibling_count",
                        "node": producer,
                        "slot": slot,
                        "field": field,
                        "value": value,
                        "window": window_name,
                    })
            checks_expected = producer_node is None or producer == producer_node
            if checks_expected and expected_siblings is not None and configured != expected_siblings:
                anomalies.append({"type": "unexpected_configured_sibling_count", "node": producer, "slot": slot, "expected": expected_siblings, "actual": configured, "window": window_name})

            sibling_ids = [str(item.get("sibling_block_id", "")) for item in identities]
            ordinary_ids = {str(item.get("ordinary_block_id", "")) for item in identities}
            if len(identities) != constructed:
                anomalies.append({"type": "sibling_identity_count_mismatch", "node": producer, "slot": slot, "constructed": constructed, "identity_events": len(identities), "window": window_name})
            if len(set(sibling_ids)) != len(sibling_ids):
                anomalies.append({"type": "duplicate_sibling_id", "node": producer, "slot": slot, "sibling_ids": sibling_ids, "window": window_name})
            if len(ordinary_ids) > 1:
                anomalies.append({"type": "multiple_ordinary_ids_for_opportunity", "node": producer, "slot": slot, "ordinary_block_ids": sorted(ordinary_ids), "window": window_name})
            ordinary_id = next(iter(ordinary_ids), None)
            if ordinary_id and ordinary_id in sibling_ids:
                anomalies.append({"type": "sibling_id_equals_ordinary_id", "node": producer, "slot": slot, "ordinary_block_id": ordinary_id, "window": window_name})

            indexes: list[int] = []
            siblings: list[dict[str, Any]] = []
            for identity in identities:
                sibling_id = str(identity.get("sibling_block_id", ""))
                if sibling_id in seen_sibling_ids:
                    anomalies.append({"type": "duplicate_sibling_id_across_opportunities", "sibling_block_id": sibling_id, "node": producer, "window": window_name})
                seen_sibling_ids.add(sibling_id)
                index = as_int(identity, "sibling_index", anomalies, identity)
                if index is not None:
                    indexes.append(index)
                if str(identity.get("parent_block_id")) != parent or str(identity.get("slot")) != slot:
                    anomalies.append({"type": "sibling_parent_or_slot_mismatch", "node": producer, "slot": slot, "sibling_block_id": sibling_id, "window": window_name})
                if ordinary_id and identity.get("ordinary_block_id") != ordinary_id:
                    anomalies.append({"type": "sibling_ordinary_id_mismatch", "node": producer, "slot": slot, "sibling_block_id": sibling_id, "window": window_name})
                relay_enqueued = truthy(identity.get("relay_enqueued"))
                candidate_receiver_events = [
                    entry
                    for entry in receiver_events
                    if entry.get("window") == window_name
                    and str(entry.get("sibling_block_id") or entry.get("block_id") or "") == sibling_id
                    and entry.get("node") != producer
                ]
                if not candidate_receiver_events:
                    incomplete_observations.append({
                        "type": "receiver_observation_missing",
                        "window": window_name,
                        "sibling_block_id": sibling_id,
                        "relay_enqueued": relay_enqueued,
                    })
                receiver_nodes: dict[str, dict[str, list[dict[str, Any]]]] = defaultdict(
                    lambda: {"matching": [], "mismatched": []}
                )
                for entry in candidate_receiver_events:
                    actual_parent = str(entry.get("parent_block_id", ""))
                    actual_slot = str(entry.get("slot", ""))
                    if actual_parent != str(identity.get("parent_block_id", "")) or actual_slot != str(identity.get("slot", "")):
                        receiver_nodes[entry["node"]]["mismatched"].append(entry)
                        anomalies.append({
                            "type": "receiver_parent_or_slot_mismatch",
                            "node": entry["node"],
                            "window": window_name,
                            "sibling_block_id": sibling_id,
                            "expected_parent_block_id": identity.get("parent_block_id"),
                            "observed_parent_block_id": entry.get("parent_block_id"),
                            "expected_slot": identity.get("slot"),
                            "observed_slot": entry.get("slot"),
                            "event": entry.get("event"),
                            "line": entry.get("line"),
                        })
                    else:
                        receiver_nodes[entry["node"]]["matching"].append(entry)

                receiver_results = []
                for receiver_node, node_events in sorted(receiver_nodes.items()):
                    matching_events = sorted(node_events["matching"], key=_event_sort_key)
                    mismatched_events = sorted(node_events["mismatched"], key=_event_sort_key)
                    stage_names = [event["stage"] for event in matching_events]
                    unique_stages = set(stage_names)
                    ordered_path = _ordered_receiver_path(matching_events)
                    if not ordered_path and matching_events:
                        missing_stages = [stage for stage in REQUIRED_RECEIVER_PATH if stage not in unique_stages]
                        if missing_stages:
                            incomplete_observations.append({
                                "type": "receiver_intermediate_stage_missing",
                                "node": receiver_node,
                                "window": window_name,
                                "sibling_block_id": sibling_id,
                                "missing_stages": missing_stages,
                            })
                        elif not missing_stages:
                            incomplete_observations.append({
                                "type": "receiver_event_order_invalid",
                                "node": receiver_node,
                                "window": window_name,
                                "sibling_block_id": sibling_id,
                            })
                    for stage in unique_stages:
                        stage_count = stage_names.count(stage)
                        if stage_count > 1:
                            incomplete_observations.append({
                                "type": "duplicate_receiver_stage_event",
                                "node": receiver_node,
                                "window": window_name,
                                "sibling_block_id": sibling_id,
                                "stage": stage,
                                "count": stage_count,
                            })
                    if "reconstructed" in unique_stages and "reconstruction_failed" in unique_stages:
                        anomalies.append({"type": "conflicting_reconstruction_events", "node": receiver_node, "window": window_name, "sibling_block_id": sibling_id})
                    if "accepted" in unique_stages and ("rejected" in unique_stages or "apply_failed" in unique_stages):
                        anomalies.append({"type": "conflicting_application_events", "node": receiver_node, "window": window_name, "sibling_block_id": sibling_id})
                    status = _receiver_status(matching_events)
                    status.update({
                        "node": receiver_node,
                        "parent_block_id": identity.get("parent_block_id"),
                        "slot": identity.get("slot"),
                        "correlation_valid": bool(matching_events),
                        "mismatched_events": [_public_event(event) for event in mismatched_events],
                    })
                    receiver_results.append(status)

                aggregate_stages = {
                    event["stage"]
                    for node_events in receiver_nodes.values()
                    for event in node_events["matching"]
                }
                complete_receiver = any(item["ordered_blend_to_acceptance"] for item in receiver_results)
                blend_nodes = {item["node"] for item in receiver_results if item["blend_received"]}
                accepted_nodes = {item["node"] for item in receiver_results if item["accepted"]}
                if blend_nodes and accepted_nodes and not complete_receiver and blend_nodes.isdisjoint(accepted_nodes):
                    incomplete_observations.append({
                        "type": "reception_and_acceptance_split_across_nodes",
                        "window": window_name,
                        "sibling_block_id": sibling_id,
                        "reception_nodes": sorted(blend_nodes),
                        "acceptance_nodes": sorted(accepted_nodes),
                    })
                if not complete_receiver and set(REQUIRED_RECEIVER_PATH).issubset(aggregate_stages):
                    incomplete_observations.append({
                        "type": "receiver_stages_split_across_nodes_or_unordered",
                        "window": window_name,
                        "sibling_block_id": sibling_id,
                    })
                siblings.append({
                    "sibling_index": index,
                    "sibling_block_id": sibling_id,
                    "parent_block_id": identity.get("parent_block_id"),
                    "slot": identity.get("slot"),
                    "relay_enqueued": relay_enqueued,
                    "identity_event": True,
                    "outcome": (
                        "accepted_after_blend_reception"
                        if complete_receiver
                        else "accepted_without_ordered_blend_evidence"
                        if "accepted" in aggregate_stages
                        else "reconstruction_failed"
                        if "reconstruction_failed" in aggregate_stages
                        else "proposal_rejected"
                        if "rejected" in aggregate_stages
                        else "application_failed"
                        if "apply_failed" in aggregate_stages
                        else "not_processed"
                        if "not_processed" in aggregate_stages
                        else "receiver_observation_incomplete"
                        if aggregate_stages
                        else "local_enqueue_succeeded_peer_delivery_not_observed"
                        if relay_enqueued
                        else "local_relay_enqueue_failed"
                    ),
                    "receiver_evidence": {
                        "blend_received": "blend_received" in aggregate_stages,
                        "blend_status": "received" if "blend_received" in aggregate_stages else "not_observed",
                        "chain_proposal_received": "chain_proposal_received" in aggregate_stages,
                        "reconstructed": "reconstructed" in aggregate_stages,
                        "reconstruction_failed": "reconstruction_failed" in aggregate_stages,
                        "reconstruction_status": _receiver_status([event for node_events in receiver_nodes.values() for event in node_events["matching"]])["reconstruction_status"],
                        "accepted": "accepted" in aggregate_stages,
                        "rejected": "rejected" in aggregate_stages,
                        "apply_failed": "apply_failed" in aggregate_stages,
                        "not_processed": "not_processed" in aggregate_stages,
                        "application_status": _receiver_status([event for node_events in receiver_nodes.values() for event in node_events["matching"]])["application_status"],
                        "stored": "storage_observed" in aggregate_stages,
                        "canonical_status": _canonical_status([event for node_events in receiver_nodes.values() for event in node_events["matching"]]),
                        "ordered_blend_to_acceptance": complete_receiver,
                        "receiver_results": receiver_results,
                        "evidence_events": [
                            _public_event(entry)
                            for entry in sorted(
                                [event for node_events in receiver_nodes.values() for event in node_events["matching"]],
                                key=_event_sort_key,
                            )
                        ],
                    },
                })

            if sorted(indexes) != list(range(1, len(identities) + 1)):
                anomalies.append({"type": "sibling_indexes_not_unique_and_sequential", "node": producer, "slot": slot, "indexes": sorted(indexes), "window": window_name})
            if relay_count is not None and sum(sibling["relay_enqueued"] for sibling in siblings) != relay_count:
                anomalies.append({"type": "relay_enqueue_count_mismatch", "node": producer, "slot": slot, "timing_count": relay_count, "identity_true_count": sum(sibling["relay_enqueued"] for sibling in siblings), "window": window_name})
            if configured is not None and constructed is not None:
                skipped = int(row.get("skipped_siblings", 0))
                if constructed > configured or constructed + skipped > configured or (failed is not None and failed > configured):
                    anomalies.append({"type": "configured_sibling_outcome_count_mismatch", "node": producer, "slot": slot, "configured": configured, "constructed": constructed, "failed": failed, "skipped": skipped, "window": window_name})

            incomplete_timing_fields = {
                field
                for issue in anomalies
                if issue.get("type") == "incomplete_timing_event"
                for field in issue.get("missing_fields", [])
            }
            for metric in METRICS:
                try:
                    value = float(row[metric])
                    if not math.isfinite(value) or value < 0:
                        raise ValueError("timing metric must be finite and nonnegative")
                    metric_values[(window_name, configured or 0, metric)].append(value)
                except (KeyError, TypeError, ValueError):
                    if metric not in incomplete_timing_fields:
                        anomalies.append({"type": "invalid_timing_metric", "node": producer, "slot": slot, "metric": metric, "value": row.get(metric), "window": window_name})

            opportunities.append({
                "window": window_name,
                "producer_node": producer,
                "parent_block_id": parent,
                "slot": slot,
                "ordinary_block_id": ordinary_id,
                "configured_siblings": configured,
                "constructed_siblings": constructed,
                "failed_siblings": failed,
                "relay_enqueued_siblings": relay_count,
                "ordinary_self_applied": truthy(row.get("ordinary_self_applied")),
                "ordinary_relay_enqueued": truthy(row.get("ordinary_relay_enqueued")),
                "timing_ms": {metric: _float_or_none(row.get(metric)) for metric in METRICS},
                "siblings": sorted(siblings, key=lambda sibling: sibling["sibling_index"] or 0),
            })

    for key, identities in identity_groups.items():
        if key not in matched_identity_keys:
            for identity in identities:
                anomalies.append({"type": "sibling_identity_without_timing_event", "node": identity["node"], "line": identity["line"], "sibling_block_id": identity.get("sibling_block_id"), "window": key[0]})

    grouped_statistics: dict[str, dict[str, Any]] = defaultdict(dict)
    for (window_name, configured, metric), values in sorted(metric_values.items()):
        if not values:
            continue
        grouped_statistics[window_name][str(configured)] = grouped_statistics[window_name].get(str(configured), {})
        grouped_statistics[window_name][str(configured)][metric] = summarize_values(values)

    stats = []
    for window_name in sorted(grouped_statistics):
        for configured in sorted(grouped_statistics[window_name], key=int):
            for metric, values in grouped_statistics[window_name][configured].items():
                stats.append({"window": window_name, "configured_siblings": int(configured), "metric": metric, **values})

    return {
        "schema_version": 1,
        "provenance": {},
        "windows": [
            {"name": window["name"], "start": window["start"].isoformat().replace("+00:00", "Z"), "end": window["end"].isoformat().replace("+00:00", "Z")}
            for window in windows
        ],
        "summary": {
            "leadership_opportunities": len(opportunities),
            "constructed_siblings": sum(opportunity["constructed_siblings"] or 0 for opportunity in opportunities),
            "failed_siblings": sum(opportunity["failed_siblings"] or 0 for opportunity in opportunities),
            "relay_enqueued_siblings": sum(opportunity["relay_enqueued_siblings"] or 0 for opportunity in opportunities),
            "observed_blend_receipts": sum(sibling["receiver_evidence"]["blend_received"] for opportunity in opportunities for sibling in opportunity["siblings"]),
            "observed_reconstructions": sum(sibling["receiver_evidence"]["reconstructed"] for opportunity in opportunities for sibling in opportunity["siblings"]),
            "observed_acceptances": sum(sibling["receiver_evidence"]["accepted"] for opportunity in opportunities for sibling in opportunity["siblings"]),
            "accepted_after_blend_reception": sum(sibling["receiver_evidence"]["ordered_blend_to_acceptance"] for opportunity in opportunities for sibling in opportunity["siblings"]),
            "complete_blend_to_acceptance_paths": sum(sibling["receiver_evidence"]["ordered_blend_to_acceptance"] for opportunity in opportunities for sibling in opportunity["siblings"]),
            "accepted_without_ordered_blend_evidence": sum(sibling["outcome"] == "accepted_without_ordered_blend_evidence" for opportunity in opportunities for sibling in opportunity["siblings"]),
            "partial_receiver_observations": sum(
                bool(receiver.get("evidence_events")) and not receiver.get("ordered_blend_to_acceptance")
                for opportunity in opportunities
                for sibling in opportunity["siblings"]
                for receiver in sibling["receiver_evidence"]["receiver_results"]
            ),
            "siblings_without_receiver_observations": sum(
                not sibling["receiver_evidence"]["receiver_results"]
                for opportunity in opportunities
                for sibling in opportunity["siblings"]
            ),
            "explicit_reconstruction_failures": sum(
                receiver["reconstruction_failed"]
                for opportunity in opportunities
                for sibling in opportunity["siblings"]
                for receiver in sibling["receiver_evidence"]["receiver_results"]
            ),
            "explicit_application_failures": sum(
                receiver["rejected"] or receiver["apply_failed"]
                for opportunity in opportunities
                for sibling in opportunity["siblings"]
                for receiver in sibling["receiver_evidence"]["receiver_results"]
            ),
            "explicit_proposal_rejections": sum(
                receiver["rejected"]
                for opportunity in opportunities
                for sibling in opportunity["siblings"]
                for receiver in sibling["receiver_evidence"]["receiver_results"]
            ),
            "explicit_consensus_application_failures": sum(
                receiver["apply_failed"]
                for opportunity in opportunities
                for sibling in opportunity["siblings"]
                for receiver in sibling["receiver_evidence"]["receiver_results"]
            ),
            "local_relay_enqueue_failures": sum(
                not sibling["relay_enqueued"]
                for opportunity in opportunities
                for sibling in opportunity["siblings"]
            ),
        },
        "statistics": stats,
        "opportunities": opportunities,
        "anomalies": anomalies,
        "incomplete_observations": incomplete_observations,
    }


def _float_or_none(value: Any) -> float | None:
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def _canonical_status(stages: list[dict[str, Any]]) -> str:
    records = [entry for entry in stages if entry.get("stage") == "storage_observed" and "canonical" in entry]
    if any(truthy(entry.get("canonical")) for entry in records):
        return "canonical"
    if records:
        return "not_canonical"
    return "unknown"


def parse_log_spec(value: str) -> tuple[str, Path]:
    if "=" not in value:
        path = Path(value)
        return path.stem, path
    node, path_text = value.split("=", 1)
    if not node or not path_text:
        raise argparse.ArgumentTypeError("logs must use NODE=PATH")
    return node, Path(path_text)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--log", action="append", required=True, metavar="NODE=PATH", help="node log; repeat for producer and receiver logs")
    parser.add_argument("--window", nargs=2, action="append", default=[], metavar=("START", "END"), help="UTC/offset ISO-8601 range [START, END); repeatable")
    parser.add_argument("--expected-siblings", type=int, help="expected configured count; checks every timing log unless --producer-node is set")
    parser.add_argument("--producer-node", help="log label whose configured count is checked by --expected-siblings")
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--commit-sha")
    parser.add_argument("--binary-sha256")
    parser.add_argument("--config-sha256")
    parser.add_argument("--feature")
    parser.add_argument("--scenario")
    parser.add_argument("--node-identity", action="append", default=[], metavar="NODE=IDENTITY")
    parser.add_argument("--allow-anomalies", action="store_true", help="write outputs and return success when anomalies are present")
    args = parser.parse_args(argv)

    if args.expected_siblings is not None and args.expected_siblings < 0:
        parser.error("--expected-siblings cannot be negative")
    try:
        windows = [parse_window(start, end, index) for index, (start, end) in enumerate(args.window, start=1)]
        log_specs = [parse_log_spec(value) for value in args.log]
    except (ValueError, argparse.ArgumentTypeError) as error:
        parser.error(str(error))

    events: list[dict[str, Any]] = []
    read_anomalies: list[dict[str, Any]] = []
    read_incomplete: list[dict[str, Any]] = []
    for node, path in log_specs:
        parsed, issues = parse_log(node, path)
        events.extend(parsed)
        read_anomalies.extend(
            issue for issue in issues if issue.get("type") != "event_missing_timestamp"
        )
        read_incomplete.extend(
            issue for issue in issues if issue.get("type") == "event_missing_timestamp"
        )

    result = analyze(events, windows, args.expected_siblings, args.producer_node)
    result["anomalies"][:0] = read_anomalies
    result["incomplete_observations"][:0] = read_incomplete
    result["provenance"] = {
        "commit_sha": args.commit_sha,
        "binary_sha256": args.binary_sha256,
        "config_sha256": args.config_sha256,
        "feature": args.feature,
        "scenario": args.scenario,
        "producer_node": args.producer_node,
        "node_identities": dict(item.split("=", 1) for item in args.node_identity if "=" in item),
        "logs": [{"node": node, "path": str(path)} for node, path in log_specs],
    }

    args.output_dir.mkdir(parents=True, exist_ok=True)
    (args.output_dir / "measurements.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    with (args.output_dir / "timings.csv").open("w", encoding="utf-8", newline="") as csv_file:
        writer = csv.DictWriter(csv_file, fieldnames=("window", "configured_siblings", "samples", "metric", "min_ms", "median_ms", "max_ms"))
        writer.writeheader()
        for row in result["statistics"]:
            writer.writerow({
                "window": row["window"],
                "configured_siblings": row["configured_siblings"],
                "samples": row["samples"],
                "metric": row["metric"],
                "min_ms": row["min"],
                "median_ms": row["median"],
                "max_ms": row["max"],
            })

    print(json.dumps({"summary": result["summary"], "anomalies": result["anomalies"], "incomplete_observations": result["incomplete_observations"], "outputs": [str(args.output_dir / "measurements.json"), str(args.output_dir / "timings.csv")]}, indent=2))
    return 1 if result["anomalies"] and not args.allow_anomalies else 0


if __name__ == "__main__":
    sys.exit(main())
