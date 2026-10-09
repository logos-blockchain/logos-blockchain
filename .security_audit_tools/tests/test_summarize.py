import contextlib
import copy
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).parents[1]))
import summarize


FIXTURE = Path(__file__).parent / "fixtures" / "sample_events.jsonl"


class SummarizeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.events, cls.parse_anomalies = summarize.parse_log("fixture-node", FIXTURE)
        if cls.parse_anomalies:
            raise AssertionError(cls.parse_anomalies)

    @staticmethod
    def receiver_event(stage, node, second, parent="0xparent1", slot=11, line=100):
        event_names = {
            "blend_received": "security_audit_blend_payload_decapsulated",
            "chain_proposal_received": "security_audit_chain_proposal_received",
            "reconstructed": "security_audit_proposal_reconstructed",
            "reconstruction_failed": "security_audit_proposal_reconstruction_failed",
            "accepted": "security_audit_proposal_accepted",
            "rejected": "security_audit_proposal_rejected",
            "apply_failed": "security_audit_proposal_apply_failed",
            "storage_observed": "security_audit_block_storage_observed",
        }
        stamp = f"2026-10-11T10:01:{second:02}.000Z"
        return {
            "timestamp": stamp,
            "_timestamp": summarize.parse_timestamp(stamp),
            "event": event_names[stage],
            "node": node,
            "path": f"{node}.log",
            "line": line + second,
            "block_id": "0xsibling1",
            "parent_block_id": parent,
            "slot": slot,
        }

    def events_with_receiver_path(self, stages, node="NODE_2"):
        base = copy.deepcopy(
            [
                event
                for event in self.events
                if event.get("slot") in ("11", 11)
                and event.get("event") not in summarize.RECEIVER_STAGES
            ]
        )
        base.extend(
            self.receiver_event(stage, node, second)
            for second, stage in enumerate(stages, start=1)
        )
        return base

    def test_zero_one_and_three_sibling_summaries(self):
        result = summarize.analyze(self.events, [], None)

        self.assertEqual(result["summary"]["leadership_opportunities"], 3)
        self.assertEqual(result["summary"]["constructed_siblings"], 4)
        self.assertEqual([row["configured_siblings"] for row in result["statistics"] if row["metric"] == "additional_processing_ms"], [0, 1, 3])
        self.assertEqual([row["samples"] for row in result["statistics"] if row["metric"] == "additional_processing_ms"], [1, 1, 1])
        self.assertEqual(result["anomalies"], [])

        one = next(item for item in result["opportunities"] if item["configured_siblings"] == 1)
        self.assertEqual(one["siblings"][0]["sibling_block_id"], "0xsibling1")
        self.assertTrue(one["siblings"][0]["receiver_evidence"]["blend_received"])
        self.assertTrue(one["siblings"][0]["receiver_evidence"]["reconstructed"])
        self.assertTrue(one["siblings"][0]["receiver_evidence"]["accepted"])

        zero = next(item for item in result["opportunities"] if item["configured_siblings"] == 0)
        self.assertEqual(zero["siblings"], [])

        three = next(item for item in result["opportunities"] if item["configured_siblings"] == 3)
        self.assertEqual([sibling["sibling_index"] for sibling in three["siblings"]], [1, 2, 3])
        self.assertEqual(len({sibling["sibling_block_id"] for sibling in three["siblings"]}), 3)
        self.assertEqual(three["siblings"][0]["receiver_evidence"]["blend_status"], "not_observed")
        self.assertEqual(three["siblings"][0]["receiver_evidence"]["application_status"], "not_observed")
        self.assertEqual(result["summary"]["accepted_after_blend_reception"], 1)
        self.assertEqual(result["summary"]["complete_blend_to_acceptance_paths"], 1)
        self.assertTrue(one["siblings"][0]["receiver_evidence"]["ordered_blend_to_acceptance"])
        self.assertEqual(one["siblings"][0]["receiver_evidence"]["receiver_results"][0]["node"], "NODE_2")

    def test_duplicate_sibling_id_is_reported(self):
        events = copy.deepcopy(self.events)
        identities = [
            event
            for event in events
            if event.get("event") == summarize.SIBLING_EVENT and event.get("slot") == 13
        ]
        identities[1]["sibling_block_id"] = identities[0]["sibling_block_id"]

        result = summarize.analyze(events, [], None)

        self.assertIn("duplicate_sibling_id", {item["type"] for item in result["anomalies"]})

    def test_missing_sibling_identity_is_reported(self):
        events = [
            event
            for event in self.events
            if not (event.get("event") == summarize.SIBLING_EVENT and event.get("sibling_index") == 2)
        ]

        result = summarize.analyze(events, [], None)

        self.assertIn("sibling_identity_count_mismatch", {item["type"] for item in result["anomalies"]})

    def test_expected_count_can_be_limited_to_the_selected_producer(self):
        events = copy.deepcopy(
            [event for event in self.events if event.get("slot") in ("11", 11)]
        )
        producer_timing = next(
            event for event in events if event.get("event") == summarize.TIMING_EVENT
        )
        peer_timing = copy.deepcopy(producer_timing)
        peer_timing["node"] = "NODE_2"
        peer_timing["configured_siblings"] = 0
        peer_timing["constructed_siblings"] = 0
        peer_timing["failed_siblings"] = 0
        peer_timing["relay_enqueued_siblings"] = 0
        events.append(peer_timing)

        targeted = summarize.analyze(events, [], 1, "NODE_1")
        global_check = summarize.analyze(events, [], 1)

        self.assertNotIn(
            "unexpected_configured_sibling_count",
            {item["type"] for item in targeted["anomalies"]},
        )
        self.assertIn(
            "unexpected_configured_sibling_count",
            {item["type"] for item in global_check["anomalies"]},
        )

    def test_failed_local_relay_is_not_peer_delivery(self):
        events = copy.deepcopy([event for event in self.events if event.get("slot") in ("11", 11)])
        timing = next(event for event in events if event.get("event") == summarize.TIMING_EVENT)
        identity = next(event for event in events if event.get("event") == summarize.SIBLING_EVENT)
        timing["relay_enqueued_siblings"] = 0
        timing["failed_siblings"] = 1
        identity["relay_enqueued"] = False
        events = [event for event in events if event.get("node") == "NODE_1"]

        result = summarize.analyze(events, [], None)
        sibling = result["opportunities"][0]["siblings"][0]

        self.assertNotIn("local_relay_enqueue_failed", {item["type"] for item in result["anomalies"]})
        self.assertFalse(sibling["relay_enqueued"])
        self.assertFalse(sibling["receiver_evidence"]["blend_received"])
        self.assertFalse(sibling["receiver_evidence"]["accepted"])
        self.assertEqual(sibling["outcome"], "local_relay_enqueue_failed")
        self.assertEqual(result["summary"]["local_relay_enqueue_failures"], 1)

    def test_reverse_receiver_event_order_is_not_accepted(self):
        events = self.events_with_receiver_path(
            ["accepted", "reconstructed", "chain_proposal_received", "blend_received"]
        )

        result = summarize.analyze(events, [], None)
        sibling = result["opportunities"][0]["siblings"][0]

        self.assertEqual(sibling["outcome"], "accepted_without_ordered_blend_evidence")
        self.assertFalse(sibling["receiver_evidence"]["ordered_blend_to_acceptance"])
        self.assertIn(
            "receiver_event_order_invalid",
            {item["type"] for item in result["incomplete_observations"]},
        )
        self.assertEqual(result["anomalies"], [])

    def test_receiver_stages_from_different_nodes_are_not_combined(self):
        events = self.events_with_receiver_path(["blend_received"], node="NODE_2")
        events.extend(
            self.receiver_event(stage, "NODE_3", second)
            for second, stage in enumerate(
                ["chain_proposal_received", "reconstructed", "accepted"], start=2
            )
        )

        result = summarize.analyze(events, [], None)
        sibling = result["opportunities"][0]["siblings"][0]

        self.assertFalse(sibling["receiver_evidence"]["ordered_blend_to_acceptance"])
        self.assertIn(
            "reception_and_acceptance_split_across_nodes",
            {item["type"] for item in result["incomplete_observations"]},
        )
        self.assertEqual(
            {receiver["node"] for receiver in sibling["receiver_evidence"]["receiver_results"]},
            {"NODE_2", "NODE_3"},
        )

    def test_missing_reconstruction_is_incomplete_not_rejection(self):
        events = self.events_with_receiver_path(
            ["blend_received", "chain_proposal_received", "accepted"]
        )

        result = summarize.analyze(events, [], None)
        sibling = result["opportunities"][0]["siblings"][0]
        receiver = sibling["receiver_evidence"]["receiver_results"][0]

        self.assertEqual(receiver["reconstruction_status"], "not_observed")
        self.assertEqual(receiver["application_status"], "accepted")
        self.assertEqual(sibling["outcome"], "accepted_without_ordered_blend_evidence")
        self.assertIn(
            "receiver_intermediate_stage_missing",
            {item["type"] for item in result["incomplete_observations"]},
        )
        self.assertNotIn("receiver_intermediate_stage_missing", {item["type"] for item in result["anomalies"]})

    def test_partial_receiver_evidence_is_nonfatal_and_not_rejection(self):
        events = self.events_with_receiver_path(
            ["chain_proposal_received", "reconstructed", "accepted"]
        )

        result = summarize.analyze(events, [], None)
        sibling = result["opportunities"][0]["siblings"][0]
        receiver = sibling["receiver_evidence"]["receiver_results"][0]

        self.assertEqual(result["anomalies"], [])
        self.assertEqual(sibling["outcome"], "accepted_without_ordered_blend_evidence")
        self.assertEqual(receiver["outcome"], "accepted")
        self.assertFalse(receiver["blend_received"])
        self.assertFalse(receiver["reconstruction_failed"])
        self.assertIn(
            "receiver_intermediate_stage_missing",
            {item["type"] for item in result["incomplete_observations"]},
        )

    def test_missing_blend_evidence_is_not_a_rejection(self):
        events = self.events_with_receiver_path(
            ["chain_proposal_received", "reconstructed", "accepted"]
        )

        sibling = summarize.analyze(events, [], None)["opportunities"][0]["siblings"][0]

        self.assertEqual(sibling["outcome"], "accepted_without_ordered_blend_evidence")
        self.assertFalse(sibling["receiver_evidence"]["rejected"])
        self.assertFalse(sibling["receiver_evidence"]["reconstruction_failed"])

    def test_reconstruction_failure_preserves_reason_as_confirmed_outcome(self):
        events = self.events_with_receiver_path(
            ["blend_received", "chain_proposal_received", "reconstruction_failed"]
        )
        failed = next(
            event
            for event in events
            if event.get("event") == "security_audit_proposal_reconstruction_failed"
        )
        failed["reason"] = "UnresolvedReference: transaction hash not in mempool"

        result = summarize.analyze(events, [], None)
        sibling = result["opportunities"][0]["siblings"][0]
        receiver = sibling["receiver_evidence"]["receiver_results"][0]

        self.assertEqual(sibling["outcome"], "reconstruction_failed")
        self.assertEqual(receiver["outcome"], "reconstruction_failed")
        self.assertEqual(result["summary"]["explicit_reconstruction_failures"], 1)
        self.assertEqual(
            receiver["reconstruction_failure_reasons"][0]["reason"],
            "UnresolvedReference: transaction hash not in mempool",
        )

    def test_confirmed_reconstruction_and_application_failures_are_reported(self):
        events = self.events_with_receiver_path(
            ["blend_received", "chain_proposal_received", "reconstruction_failed", "rejected"]
        )

        result = summarize.analyze(events, [], None)
        receiver = result["opportunities"][0]["siblings"][0]["receiver_evidence"]["receiver_results"][0]

        self.assertEqual(receiver["reconstruction_status"], "failed")
        self.assertEqual(receiver["application_status"], "rejected")
        self.assertEqual(result["opportunities"][0]["siblings"][0]["outcome"], "reconstruction_failed")

    def test_storage_observation_is_reported_per_receiving_node(self):
        events = self.events_with_receiver_path(
            ["blend_received", "chain_proposal_received", "reconstructed", "accepted"]
        )
        events.append(
            self.receiver_event("storage_observed", "NODE_2", 5, line=200)
        )

        result = summarize.analyze(events, [], None)
        receiver = result["opportunities"][0]["siblings"][0]["receiver_evidence"]["receiver_results"][0]

        self.assertTrue(receiver["stored"])
        self.assertEqual(receiver["canonical_status"], "unknown")

    def test_receiver_parent_and_slot_mismatches_are_reported_and_excluded(self):
        events = self.events_with_receiver_path(
            ["blend_received", "chain_proposal_received", "reconstructed", "accepted"]
        )
        for event in events:
            if event.get("event") == "security_audit_chain_proposal_received":
                event["slot"] = 12
            if event.get("event") == "security_audit_blend_payload_decapsulated":
                event["parent_block_id"] = "0xwrong-parent"

        result = summarize.analyze(events, [], None)
        sibling = result["opportunities"][0]["siblings"][0]

        self.assertFalse(sibling["receiver_evidence"]["ordered_blend_to_acceptance"])
        self.assertEqual(
            {item["type"] for item in result["anomalies"]}.intersection(
                {"receiver_parent_or_slot_mismatch"}
            ),
            {"receiver_parent_or_slot_mismatch"},
        )
        receiver = sibling["receiver_evidence"]["receiver_results"][0]
        self.assertEqual(len(receiver["mismatched_events"]), 2)

    def test_repeated_events_require_a_valid_ordered_subsequence(self):
        valid_events = self.events_with_receiver_path(
            ["blend_received", "blend_received", "chain_proposal_received", "reconstructed", "accepted"]
        )
        valid = summarize.analyze(valid_events, [], None)
        self.assertTrue(valid["opportunities"][0]["siblings"][0]["receiver_evidence"]["ordered_blend_to_acceptance"])

        invalid_events = self.events_with_receiver_path(
            ["blend_received", "reconstructed", "chain_proposal_received", "accepted", "chain_proposal_received"]
        )
        invalid = summarize.analyze(invalid_events, [], None)
        self.assertFalse(invalid["opportunities"][0]["siblings"][0]["receiver_evidence"]["ordered_blend_to_acceptance"])
        anomaly_types = {item["type"] for item in invalid["anomalies"]}
        self.assertNotIn("duplicate_receiver_stage_event", anomaly_types)
        self.assertNotIn("receiver_event_order_invalid", anomaly_types)
        incomplete_types = {item["type"] for item in invalid["incomplete_observations"]}
        self.assertIn("duplicate_receiver_stage_event", incomplete_types)
        self.assertIn("receiver_event_order_invalid", incomplete_types)

    def test_receiver_evidence_is_scoped_to_measurement_window(self):
        events = self.events_with_receiver_path(
            ["blend_received", "chain_proposal_received", "reconstructed", "accepted"]
        )
        window = summarize.parse_window(
            "2026-10-11T10:02:00Z", "2026-10-11T10:03:00Z", 1
        )

        result = summarize.analyze(events, [window], None)

        self.assertEqual(result["summary"]["leadership_opportunities"], 0)
        self.assertEqual(result["summary"]["accepted_after_blend_reception"], 0)

    def test_incomplete_timing_and_identity_records_are_reported(self):
        events = copy.deepcopy(self.events)
        timing = next(event for event in events if event.get("event") == summarize.TIMING_EVENT)
        del timing["ordinary_proposal_ms"]
        identity = next(event for event in events if event.get("event") == summarize.SIBLING_EVENT)
        del identity["sibling_index"]

        result = summarize.analyze(events, [], None)
        anomaly_types = {item["type"] for item in result["anomalies"]}

        self.assertIn("incomplete_timing_event", anomaly_types)
        self.assertIn("incomplete_sibling_identity", anomaly_types)

    def test_invalid_timing_values_remain_actionable_errors(self):
        events = copy.deepcopy(self.events)
        timing = next(event for event in events if event.get("event") == summarize.TIMING_EVENT)
        timing["additional_processing_ms"] = float("nan")

        result = summarize.analyze(events, [], None)

        self.assertIn("invalid_timing_metric", {item["type"] for item in result["anomalies"]})

    def test_window_is_half_open(self):
        window = summarize.parse_window("2026-10-11T10:01:00Z", "2026-10-11T10:02:00Z", 1)

        result = summarize.analyze(self.events, [window], None)

        self.assertEqual(result["summary"]["leadership_opportunities"], 1)
        self.assertEqual(result["opportunities"][0]["configured_siblings"], 1)

    def test_logfmt_parser_handles_existing_node_event_shape(self):
        line = (
            '2026-10-11T10:00:00.000Z INFO target: message '
            'diagnostic="blend_reachability" event="security_audit_sibling_submitted" '
            'ordinary_block_id=0xordinary sibling_block_id=0xsibling '
            'parent_block_id=0xparent slot=42 sibling_index=1 relay_enqueued=true'
        )

        fields = summarize.parse_fields(line)

        self.assertEqual(fields["event"], summarize.SIBLING_EVENT)
        self.assertEqual(fields["sibling_block_id"], "0xsibling")
        self.assertTrue(summarize.truthy(fields["relay_enqueued"]))

    def test_cli_writes_json_csv_and_provenance(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "summary"
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                exit_code = summarize.main([
                    "--log", f"NODE_1={FIXTURE}",
                    "--output-dir", str(output),
                    "--commit-sha", "abc123",
                    "--binary-sha256", "binary-hash",
                    "--config-sha256", "config-hash",
                    "--producer-node", "NODE_1",
                    "--feature", "security-audit,testing",
                    "--scenario", "audit smoke",
                    "--node-identity", "NODE_1=peer-id-1",
                ])

            self.assertEqual(exit_code, 0)
            result = json.loads((output / "measurements.json").read_text())
            self.assertEqual(result["provenance"]["commit_sha"], "abc123")
            self.assertEqual(result["provenance"]["node_identities"]["NODE_1"], "peer-id-1")
            self.assertEqual(result["provenance"]["producer_node"], "NODE_1")
            csv_text = (output / "timings.csv").read_text()
            self.assertIn("configured_siblings,samples,metric", csv_text)
            self.assertIn("additional_processing_ms", csv_text)

    def test_partial_receiver_evidence_keeps_default_cli_exit_successful(self):
        events = self.events_with_receiver_path(
            ["chain_proposal_received", "reconstructed", "accepted"]
        )
        events.append({
            "event": "security_audit_diagnostic",
            "node": "NODE_2",
            "path": "NODE_2.log",
            "line": 999,
        })
        with tempfile.TemporaryDirectory() as directory:
            log_path = Path(directory) / "partial.jsonl"
            log_path.write_text(
                "\n".join(
                    json.dumps({key: value for key, value in event.items() if key != "_timestamp"})
                    for event in events
                )
                + "\n"
            )
            output = Path(directory) / "summary"
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                exit_code = summarize.main([
                    "--log", f"NODE_1={log_path}",
                    "--output-dir", str(output),
                ])

            report = json.loads((output / "measurements.json").read_text())

        self.assertEqual(exit_code, 0)
        self.assertEqual(report["anomalies"], [])
        self.assertIn(
            "receiver_intermediate_stage_missing",
            {item["type"] for item in report["incomplete_observations"]},
        )
        self.assertIn(
            "event_missing_timestamp",
            {item["type"] for item in report["incomplete_observations"]},
        )

    def test_allow_anomalies_still_returns_success(self):
        with tempfile.TemporaryDirectory() as directory:
            log_path = Path(directory) / "malformed.jsonl"
            log_path.write_text(FIXTURE.read_text() + "{broken json\n")
            output = Path(directory) / "summary"
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                exit_code = summarize.main([
                    "--log", f"NODE_1={log_path}",
                    "--output-dir", str(output),
                    "--allow-anomalies",
                ])

            report = json.loads((output / "measurements.json").read_text())

        self.assertEqual(exit_code, 0)
        self.assertIn("malformed_json_log_line", {item["type"] for item in report["anomalies"]})


if __name__ == "__main__":
    unittest.main()
