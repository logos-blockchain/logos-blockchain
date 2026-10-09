use super::{
    ancestry::self_parented_ancestry_result,
    propagation::{ReceiverEvidence, selected_sibling_reception},
    scenario::{parse_audit_participants, validate_audit_participants},
    *,
};

#[test]
fn effective_sibling_configuration_must_match_each_startup_override() {
    assert!(validate_sibling_configuration("producer", true, 4, 4).is_ok());
    assert!(validate_sibling_configuration("producer", true, 0, 0).is_ok());
    assert!(validate_sibling_configuration("producer", true, 4, 0).is_err());
    assert!(validate_sibling_configuration("honest", false, 0, 0).is_ok());
    assert!(validate_sibling_configuration("honest", false, 4, 4).is_err());
}

#[test]
fn exhausted_nonempty_workload_keeps_observing_until_deadline() {
    let start = Instant::now();
    let deadline = start + Duration::from_secs(30);
    let pending_end = start + Duration::from_secs(2);
    assert_eq!(
        nonempty_workload_action(start, deadline, None, 0, 1),
        NonemptyWorkloadAction::SubmitNext
    );
    for seconds in [2, 10, 29] {
        assert_eq!(
            nonempty_workload_action(
                start + Duration::from_secs(seconds),
                deadline,
                Some(pending_end),
                1,
                1,
            ),
            NonemptyWorkloadAction::Observe
        );
    }
    assert_eq!(
        nonempty_workload_action(deadline, deadline, Some(pending_end), 1, 1),
        NonemptyWorkloadAction::Finished
    );

    // The only qualifying block arrives after the last transaction's
    // pending window. It remains selectable before the overall deadline.
    let submitted = BTreeSet::from(["submitted".to_owned()]);
    let mut found = false;
    for (seconds, body) in [(2, vec![]), (8, vec!["submitted".to_owned()])] {
        if nonempty_workload_action(
            start + Duration::from_secs(seconds),
            deadline,
            Some(pending_end),
            1,
            1,
        ) == NonemptyWorkloadAction::Observe
            && has_nonempty_submitted_body(&body, &submitted)
        {
            found = true;
            break;
        }
    }
    assert!(found);
}

#[test]
fn nonempty_workload_schedules_next_only_within_budget_and_deadline() {
    let start = Instant::now();
    let deadline = start + Duration::from_secs(10);
    let pending_end = start + Duration::from_secs(4);
    assert_eq!(
        nonempty_workload_action(start, deadline, Some(pending_end), 1, 2),
        NonemptyWorkloadAction::Observe
    );
    assert_eq!(
        nonempty_workload_action(
            start + Duration::from_secs(1),
            deadline,
            Some(pending_end),
            1,
            2
        ),
        NonemptyWorkloadAction::Observe
    );
    assert_eq!(
        nonempty_workload_action(pending_end, deadline, Some(pending_end), 1, 2),
        NonemptyWorkloadAction::SubmitNext
    );
    assert_eq!(
        nonempty_workload_action(deadline, deadline, Some(pending_end), 1, 2),
        NonemptyWorkloadAction::Finished
    );
}

#[test]
fn nonempty_workload_paces_funded_submissions_across_the_full_window() {
    let start = Instant::now();
    let deadline = start + Duration::from_mins(15);
    let mut submitted = 0;
    let mut pending_deadline = None;
    for second in 0..=900 {
        let now = start + Duration::from_secs(second);
        match nonempty_workload_action(now, deadline, pending_deadline, submitted, 80) {
            NonemptyWorkloadAction::SubmitNext => {
                submitted += 1;
                pending_deadline = Some(now + Duration::from_secs(30));
            }
            NonemptyWorkloadAction::Observe => {}
            NonemptyWorkloadAction::Finished => assert_eq!(second, 900),
        }
    }
    assert_eq!(submitted, 30);
    assert!(submitted <= 80); // scenario workload budget
}

#[test]
fn binary_provenance_distinguishes_override_and_expected_features() {
    let selected = Path::new(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .unwrap();
    assert_eq!(
        binary_selection_mode(None, &selected),
        "framework_provider_unspecified"
    );
    assert_eq!(
        binary_selection_mode(Some(&selected), &selected),
        "environment_override_matches_selected_path"
    );
    assert_eq!(
        binary_selection_mode(Some(Path::new("/missing-audit-binary")), &selected),
        "environment_override_configured_selection_unverified"
    );
    assert_eq!(
        expected_node_build_features(&NodeBinaryProfile::Normal),
        ["testing", "security-audit"]
    );
    assert_eq!(
        expected_node_build_features(&NodeBinaryProfile::TokioConsole),
        ["testing", "security-audit", "tokio-console"]
    );
    let mut provenance = json!({"observed_audit_functionality": {"status": "not_observed_yet"}});
    record_observed_audit_opportunity(&mut provenance, 2);
    assert_eq!(
        provenance["observed_audit_functionality"]["complete_producer_opportunities"],
        2
    );
    assert_eq!(
        provenance["observed_audit_functionality"]["executable_feature_set_independently_verified"],
        false
    );
}

const SIBLING_ID: &str = "0xsibling";
const PARENT_ID: &str = "0xparent";

fn participant_rows(entries: &[(&str, &str, &str)]) -> Vec<Vec<String>> {
    std::iter::once(("node", "role", "canonical_reference"))
        .chain(entries.iter().copied())
        .map(|(node, role, reference)| vec![node.to_owned(), role.to_owned(), reference.to_owned()])
        .collect()
}

#[test]
fn participant_declarations_accept_arbitrary_cluster_names_and_honest_counts() {
    let nodes = HashSet::from(["leader".to_owned(), "alice".to_owned(), "bob".to_owned()]);
    let one = participant_rows(&[("leader", "producer", "no"), ("alice", "honest", "yes")]);
    let two_node_cluster = HashSet::from(["leader".to_owned(), "alice".to_owned()]);
    let participants = parse_audit_participants(&one).unwrap();
    assert!(validate_audit_participants(&participants, &two_node_cluster).is_ok());
    assert!(validate_audit_participants(&participants, &nodes).is_err());
    assert_eq!(participants.producer, "leader");
    assert_eq!(participants.honest_nodes, ["alice"]);
    assert_eq!(participants.canonical_reference, "alice");
    let two = participant_rows(&[
        ("leader", "producer", "no"),
        ("alice", "honest", "yes"),
        ("bob", "honest", "no"),
    ]);
    assert_eq!(
        parse_audit_participants(&two).unwrap().honest_nodes.len(),
        2
    );
}

#[test]
fn participant_declarations_reject_duplicate_missing_and_conflicting_roles() {
    let nodes = HashSet::from(["leader".to_owned(), "alice".to_owned(), "bob".to_owned()]);
    for entries in [
        vec![("leader", "producer", "no"), ("leader", "honest", "yes")],
        vec![("alice", "honest", "yes")],
        vec![("leader", "producer", "no")],
        vec![("leader", "producer", "yes"), ("alice", "honest", "yes")],
        vec![("leader", "producer", "no"), ("alice", "honest", "no")],
        vec![
            ("leader", "producer", "no"),
            ("alice", "honest", "yes"),
            ("bob", "honest", "yes"),
        ],
        vec![("leader", "producer", "no"), ("alice", "other", "yes")],
        vec![
            ("leader", "producer", "no"),
            ("bob", "producer", "no"),
            ("alice", "honest", "yes"),
        ],
    ] {
        assert!(parse_audit_participants(&participant_rows(&entries)).is_err());
    }
    let missing = parse_audit_participants(&participant_rows(&[
        ("leader", "producer", "no"),
        ("missing", "honest", "yes"),
    ]))
    .unwrap();
    assert!(validate_audit_participants(&missing, &nodes).is_err());
}

fn header_id(byte: u8) -> HeaderId {
    HeaderId::try_from([byte; 32].as_slice()).expect("32-byte test header ID")
}

fn event(
    event_name: &str,
    stage_id: &str,
    parent: &str,
    slot: &str,
    second: u32,
    offset: u64,
) -> ObservedLogLine {
    ObservedLogLine {
        text: format!(
            "2026-10-11T10:00:{second:02}.000Z INFO event=\"{event_name}\" {stage_id}=\"{SIBLING_ID}\" parent_block_id=\"{parent}\" slot=\"{slot}\""
        ),
        path: PathBuf::from("NODE_2.log"),
        offset,
    }
}

fn stage(stage: &str, second: u32, offset: u64) -> ObservedLogLine {
    let event_name = match stage {
        "blend_received" => "security_audit_blend_payload_decapsulated",
        "chain_proposal_received" => "security_audit_chain_proposal_received",
        "reconstructed" => "security_audit_proposal_reconstructed",
        "accepted" => "security_audit_proposal_accepted",
        _ => panic!("unsupported test stage: {stage}"),
    };
    event(event_name, "block_id", PARENT_ID, "42", second, offset)
}

fn matched_evidence(events: Vec<ObservedLogLine>) -> ReceiverEvidence {
    let mut logs = BTreeMap::new();
    logs.insert("NODE_2".to_owned(), events);
    let ordinary = HashMap::from([
        ("parent_block_id".to_owned(), PARENT_ID.to_owned()),
        ("slot".to_owned(), "42".to_owned()),
    ]);
    receiver_stages(
        &logs,
        &BTreeSet::from([SIBLING_ID.to_owned()]),
        &ordinary,
        &["NODE_2"],
    )
    .remove(&ReceiverIdentity {
        node: "NODE_2".to_owned(),
        sibling_id: SIBLING_ID.to_owned(),
    })
    .unwrap_or_default()
}

fn producer_log(configured: usize, indices: &[usize]) -> Vec<ObservedLogLine> {
    let mut lines = vec![format!(
        "event=security_audit_ordinary_submitted ordinary_block_id=ordinary parent_block_id=parent slot=42 configured_siblings={configured} ordinary_self_applied=true ordinary_relay_enqueued=true"
    )];
    lines.extend(indices.iter().map(|index| {
            format!(
                "event=security_audit_sibling_submitted ordinary_block_id=ordinary sibling_block_id=sibling-{index} parent_block_id=parent slot=42 sibling_index={index}"
            )
        }));
    lines.push(format!(
            "event=security_audit_leadership_timing parent_block_id=parent slot=42 configured_siblings={configured} constructed_siblings={configured} ordinary_self_applied=true ordinary_relay_enqueued=true"
        ));
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| ObservedLogLine {
            text: format!("2026-10-11T10:00:{index:02}.000Z INFO {line}"),
            path: PathBuf::from("producer.log"),
            offset: u64::try_from(index).expect("small test offset"),
        })
        .collect()
}

#[test]
fn producer_opportunities_use_the_recorded_configured_sibling_count() {
    for count in [1, 2, 4, 6] {
        let indices = (1..=count).collect::<Vec<_>>();
        let opportunities = find_producer_opportunities(&producer_log(count, &indices));
        assert_eq!(opportunities.len(), 1);
        assert_eq!(opportunities[0].siblings.len(), count);
    }
}

#[test]
fn producer_opportunity_rejects_missing_duplicate_and_out_of_range_indices() {
    for (count, indices) in [
        (0, vec![]),
        (3, vec![1, 2]),
        (3, vec![1, 1, 3]),
        (3, vec![1, 2, 4]),
    ] {
        assert!(find_producer_opportunities(&producer_log(count, &indices)).is_empty());
    }
}

#[test]
fn producer_opportunity_rejects_mismatched_timing_count() {
    let mut lines = producer_log(2, &[1, 2]);
    let timing = lines.last_mut().expect("timing event");
    timing.text = timing
        .text
        .replace("constructed_siblings=2", "constructed_siblings=3");
    assert!(find_producer_opportunities(&lines).is_empty());
}

#[test]
fn ordered_receiver_sequence_on_one_honest_node_is_accepted() {
    let evidence = matched_evidence(vec![
        stage("blend_received", 1, 1),
        stage("chain_proposal_received", 2, 2),
        stage("reconstructed", 3, 3),
        stage("accepted", 4, 4),
    ]);

    assert!(evidence.has_ordered_blend_to_acceptance());
}

#[test]
fn reverse_order_is_not_accepted() {
    let evidence = matched_evidence(vec![
        stage("accepted", 1, 1),
        stage("reconstructed", 2, 2),
        stage("chain_proposal_received", 3, 3),
        stage("blend_received", 4, 4),
    ]);

    assert!(!evidence.has_ordered_blend_to_acceptance());
}

#[test]
fn stages_from_different_honest_nodes_are_not_combined() {
    let mut logs = BTreeMap::new();
    logs.insert("NODE_2".to_owned(), vec![stage("blend_received", 1, 1)]);
    logs.insert(
        "NODE_3".to_owned(),
        vec![
            event(
                "security_audit_chain_proposal_received",
                "block_id",
                PARENT_ID,
                "42",
                2,
                2,
            ),
            event(
                "security_audit_proposal_reconstructed",
                "block_id",
                PARENT_ID,
                "42",
                3,
                3,
            ),
            event(
                "security_audit_proposal_accepted",
                "block_id",
                PARENT_ID,
                "42",
                4,
                4,
            ),
        ],
    );
    let ordinary = HashMap::from([
        ("parent_block_id".to_owned(), PARENT_ID.to_owned()),
        ("slot".to_owned(), "42".to_owned()),
    ]);
    let evidence = receiver_stages(
        &logs,
        &BTreeSet::from([SIBLING_ID.to_owned()]),
        &ordinary,
        &["NODE_2", "NODE_3"],
    );

    assert!(
        evidence
            .values()
            .all(|item| !item.has_ordered_blend_to_acceptance())
    );
}

#[test]
fn receiver_correlation_uses_only_the_supplied_honest_nodes() {
    let complete = vec![
        stage("blend_received", 1, 1),
        stage("chain_proposal_received", 2, 2),
        stage("reconstructed", 3, 3),
        stage("accepted", 4, 4),
    ];
    let logs = BTreeMap::from([
        ("producer".to_owned(), complete.clone()),
        ("peer-a".to_owned(), complete.clone()),
        ("peer-b".to_owned(), complete),
    ]);
    let ordinary = HashMap::from([
        ("parent_block_id".to_owned(), PARENT_ID.to_owned()),
        ("slot".to_owned(), "42".to_owned()),
    ]);
    let ids = BTreeSet::from([SIBLING_ID.to_owned()]);
    let one_peer = receiver_stages(&logs, &ids, &ordinary, &["peer-a"]);
    assert_eq!(one_peer.len(), 1);
    assert!(
        one_peer[&ReceiverIdentity {
            node: "peer-a".to_owned(),
            sibling_id: SIBLING_ID.to_owned(),
        }]
            .has_ordered_blend_to_acceptance()
    );
    let two_peers = receiver_stages(&logs, &ids, &ordinary, &["peer-a", "peer-b"]);
    assert_eq!(two_peers.len(), 2);
    assert!(!two_peers.contains_key(&ReceiverIdentity {
        node: "producer".to_owned(),
        sibling_id: SIBLING_ID.to_owned(),
    }));
    assert!(receiver_stages(&logs, &ids, &ordinary, &["missing"]).is_empty());
}

#[test]
fn missing_reconstruction_is_not_accepted() {
    let evidence = matched_evidence(vec![
        stage("blend_received", 1, 1),
        stage("chain_proposal_received", 2, 2),
        stage("accepted", 4, 4),
    ]);

    assert!(!evidence.has_ordered_blend_to_acceptance());
}

#[test]
fn wrong_parent_or_slot_events_are_excluded() {
    for (parent, slot) in [("0xother", "42"), (PARENT_ID, "43")] {
        let evidence = matched_evidence(vec![
            event(
                "security_audit_blend_payload_decapsulated",
                "block_id",
                parent,
                slot,
                1,
                1,
            ),
            stage("chain_proposal_received", 2, 2),
            stage("reconstructed", 3, 3),
            stage("accepted", 4, 4),
        ]);

        assert!(!evidence.has_ordered_blend_to_acceptance());
        assert_eq!(evidence.mismatched_events.len(), 1);
    }
}

#[test]
fn repeated_events_are_accepted_only_when_a_valid_ordered_path_exists() {
    let valid = matched_evidence(vec![
        stage("blend_received", 1, 1),
        stage("blend_received", 2, 2),
        stage("chain_proposal_received", 3, 3),
        stage("reconstructed", 4, 4),
        stage("accepted", 5, 5),
    ]);
    let invalid = matched_evidence(vec![
        stage("blend_received", 1, 1),
        stage("reconstructed", 2, 2),
        stage("chain_proposal_received", 3, 3),
        stage("accepted", 4, 4),
        stage("chain_proposal_received", 5, 5),
    ]);

    assert!(valid.has_ordered_blend_to_acceptance());
    assert!(!invalid.has_ordered_blend_to_acceptance());
}

#[test]
fn distinct_timestamps_order_events_across_rotated_logs() {
    let mut events = vec![
        stage("blend_received", 1, 1),
        stage("chain_proposal_received", 2, 2),
        stage("reconstructed", 3, 3),
        stage("accepted", 4, 4),
    ];
    events[0].path = PathBuf::from("NODE_2.log.1");
    events[1].path = PathBuf::from("NODE_2.log.2");
    events[2].path = PathBuf::from("NODE_2.log.1");
    events[3].path = PathBuf::from("NODE_2.log.2");

    assert!(matched_evidence(events).has_ordered_blend_to_acceptance());
}

#[test]
fn missing_timestamps_and_equal_time_cross_file_events_are_incomplete() {
    let mut missing_timestamp = vec![
        stage("blend_received", 1, 1),
        stage("chain_proposal_received", 2, 2),
        stage("reconstructed", 3, 3),
        stage("accepted", 4, 4),
    ];
    missing_timestamp[0].text = "INFO event=\"security_audit_blend_payload_decapsulated\" block_id=\"0xsibling\" parent_block_id=\"0xparent\" slot=\"42\"".to_owned();
    let tied_cross_file = vec![
        stage("blend_received", 1, 1),
        stage("chain_proposal_received", 1, 2),
        stage("reconstructed", 1, 3),
        stage("accepted", 1, 4),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, mut event)| {
        event.path = PathBuf::from(format!("NODE_2.log.{}", index % 2));
        event
    })
    .collect::<Vec<_>>();

    assert!(!matched_evidence(missing_timestamp).has_ordered_blend_to_acceptance());
    assert!(!matched_evidence(tied_cross_file).has_ordered_blend_to_acceptance());
}

fn body_evidence(block_id: &str, transaction_hashes: &[&str]) -> BlockBodyEvidence {
    BlockBodyEvidence {
        block_id: block_id.to_owned(),
        parent_block_id: PARENT_ID.to_owned(),
        slot: 42,
        body_root: "0xbody-root".to_owned(),
        voucher_commitment: "ordinary-voucher".to_owned(),
        transaction_hashes: transaction_hashes
            .iter()
            .map(|hash| (*hash).to_owned())
            .collect(),
    }
}

fn opportunity_with_siblings() -> ProducerOpportunity {
    let siblings = (1..=SIBLING_COUNT)
        .map(|index| {
            (
                index,
                HashMap::from([
                    ("sibling_block_id".to_owned(), format!("0xsibling-{index}")),
                    ("parent_block_id".to_owned(), PARENT_ID.to_owned()),
                    ("slot".to_owned(), "42".to_owned()),
                ]),
            )
        })
        .collect();
    ProducerOpportunity {
        ordinary: HashMap::from([
            ("ordinary_block_id".to_owned(), "0xordinary".to_owned()),
            ("parent_block_id".to_owned(), PARENT_ID.to_owned()),
            ("slot".to_owned(), "42".to_owned()),
            ("configured_siblings".to_owned(), SIBLING_COUNT.to_string()),
        ]),
        ordinary_line: 1,
        siblings,
        timing: HashMap::new(),
        timing_line: 10,
    }
}

#[test]
fn shutdown_trigger_requires_selected_sibling_and_matching_parent_and_slot() {
    let mut opportunity = opportunity_with_siblings();
    opportunity.siblings[0]
        .1
        .insert("sibling_block_id".to_owned(), SIBLING_ID.to_owned());
    let nodes = vec!["NODE_2".to_owned()];
    let mut logs = BTreeMap::from([(
        "NODE_2".to_owned(),
        vec![event(
            "security_audit_blend_payload_decapsulated",
            "block_id",
            "wrong",
            "42",
            1,
            1,
        )],
    )]);
    assert!(selected_sibling_reception(&logs, &opportunity, &nodes).is_none());
    logs.get_mut("NODE_2").unwrap().push(event(
        "security_audit_blend_payload_decapsulated",
        "block_id",
        PARENT_ID,
        "43",
        2,
        2,
    ));
    assert!(selected_sibling_reception(&logs, &opportunity, &nodes).is_none());
    logs.get_mut("NODE_2")
        .unwrap()
        .push(stage("blend_received", 3, 3));
    let observed = selected_sibling_reception(&logs, &opportunity, &nodes).unwrap();
    assert_eq!(
        (
            observed.node.as_str(),
            observed.sibling_id.as_str(),
            observed.event.offset,
        ),
        ("NODE_2", SIBLING_ID, 3)
    );
    assert!(selected_sibling_reception(&logs, &opportunity, &["other".to_owned()]).is_none());
    opportunity.siblings.clear();
    assert!(selected_sibling_reception(&logs, &opportunity, &nodes).is_none());
}

#[test]
fn nonempty_body_selection_rejects_empty_and_unrelated_transactions() {
    let submitted = BTreeSet::from(["0xtest-transaction".to_owned()]);

    assert!(!has_nonempty_submitted_body(&[], &submitted));
    assert!(!has_nonempty_submitted_body(
        &["0xother-transaction".to_owned()],
        &submitted
    ));
    assert!(has_nonempty_submitted_body(
        &[
            "0xother-transaction".to_owned(),
            "0xtest-transaction".to_owned()
        ],
        &submitted
    ));
}

#[test]
fn sibling_body_requires_identical_commitment_and_ordered_transaction_hashes() {
    let ordinary = body_evidence("0xordinary", &["0xtx-a", "0xtx-b"]);
    assert!(same_transaction_body(
        &ordinary,
        "0xbody-root",
        &["0xtx-a".to_owned(), "0xtx-b".to_owned()]
    ));
    assert!(!same_transaction_body(
        &ordinary,
        "0xdifferent-root",
        &["0xtx-a".to_owned(), "0xtx-b".to_owned()]
    ));
    assert!(!same_transaction_body(
        &ordinary,
        "0xbody-root",
        &["0xtx-b".to_owned(), "0xtx-a".to_owned()]
    ));
    assert!(!same_transaction_body(
        &ordinary,
        "0xbody-root",
        &["0xtx-a".to_owned(), "0xtx-c".to_owned()]
    ));
}

#[test]
fn retrieved_vouchers_must_be_distinct_and_consistent() {
    let mut seen = BTreeMap::new();
    assert!(record_voucher_commitment(
        &mut seen,
        "ordinary",
        "a",
        Some("voucher-a")
    ));
    assert!(record_voucher_commitment(
        &mut seen,
        "ordinary",
        "a",
        Some("voucher-a")
    ));
    assert!(!record_voucher_commitment(
        &mut seen,
        "ordinary",
        "b",
        Some("voucher-a")
    ));
    assert!(!record_voucher_commitment(
        &mut seen,
        "ordinary",
        "c",
        Some("ordinary")
    ));
    assert!(!record_voucher_commitment(&mut seen, "ordinary", "d", None));
    assert!(!record_voucher_commitment(
        &mut seen,
        "ordinary",
        "a",
        Some("different")
    ));
}

#[test]
fn sibling_identity_check_requires_distinct_same_parent_same_slot_ids() {
    let ordinary = body_evidence("0xordinary", &["0xtx-a"]);
    let mut opportunity = opportunity_with_siblings();
    assert!(sibling_body_integrity(&ordinary, &opportunity));

    opportunity.siblings[0]
        .1
        .insert("parent_block_id".to_owned(), "0xother-parent".to_owned());
    assert!(!sibling_body_integrity(&ordinary, &opportunity));
}

#[test]
fn configured_sibling_count_controls_identity_and_minimum_checks() {
    let ordinary = body_evidence("0xordinary", &["0xtx-a"]);
    let mut opportunity = opportunity_with_siblings();
    opportunity.siblings.truncate(2);
    opportunity
        .ordinary
        .insert("configured_siblings".to_owned(), "2".to_owned());
    assert!(sibling_body_integrity(&ordinary, &opportunity));
    assert!(valid_sibling_minimum(2, 1));
    assert!(valid_sibling_minimum(2, 2));
    assert!(!valid_sibling_minimum(2, 3));
    assert!(!valid_sibling_minimum(2, 0));
    opportunity.siblings[1]
        .1
        .insert("sibling_block_id".to_owned(), "0xsibling-1".to_owned());
    assert!(!sibling_body_integrity(&ordinary, &opportunity));
    opportunity.ordinary.remove("configured_siblings");
    assert!(!sibling_body_integrity(&ordinary, &opportunity));
}

fn attack_evidence_for_branch_checks() -> AttackEvidence {
    AttackEvidence {
        ordinary_id: header_id(1),
        ordinary_id_text: header_id(1).encode_hex::<String>(),
        parent_id: header_id(9),
        parent_id_text: header_id(9).encode_hex::<String>(),
        slot: 42,
        body_root: "test-body-root".to_owned(),
        transaction_hashes: vec!["test-transaction".to_owned()],
        siblings: (1..=SIBLING_COUNT)
            .map(|index| {
                (
                    index,
                    AttackCandidate {
                        id: header_id(index as u8 + 1),
                    },
                )
            })
            .collect(),
    }
}

#[test]
fn ancestry_presence_distinguishes_proven_absence_from_missing_evidence() {
    let target = header_id(2);
    let parent_edge_proof = AncestryPath {
        links: vec![HeaderLink {
            id: header_id(1),
            parent: target,
            slot: 43,
        }],
        target_reached: true,
        complete_below_slot: true,
        error: None,
    };
    let absent = AncestryPath {
        links: vec![HeaderLink {
            id: header_id(1),
            parent: header_id(9),
            slot: 43,
        }],
        target_reached: false,
        complete_below_slot: true,
        error: None,
    };
    let incomplete = AncestryPath {
        links: absent.links.clone(),
        target_reached: false,
        complete_below_slot: false,
        error: Some("block API unavailable".to_owned()),
    };
    assert_eq!(ancestry_presence(&parent_edge_proof, target), Some(true));
    assert_eq!(ancestry_presence(&absent, target), Some(false));
    assert_eq!(ancestry_presence(&incomplete, target), None);
}

#[test]
fn self_parented_header_is_incomplete_not_proven_ancestry_absence() {
    let self_parented = header_id(7);
    let path = self_parented_ancestry_result(
        vec![HeaderLink {
            id: self_parented,
            parent: self_parented,
            slot: 0,
        }],
        self_parented,
    );

    assert!(!path.target_reached);
    assert!(!path.complete_below_slot);
    assert!(
        path.error
            .as_deref()
            .is_some_and(|error| error.contains("self-parented"))
    );
    assert_eq!(ancestry_presence(&path, header_id(8)), None);
}

#[test]
fn pre_attack_lib_slot_proves_equivocation_is_not_finalized_without_genesis_lookup() {
    let attack = attack_evidence_for_branch_checks();
    let path = proven_below_target_slot_path(0, attack.slot)
        .expect("a LIB below the attack slot proves it cannot contain the attack block");

    assert_eq!(ancestry_presence(&path, attack.parent_id), Some(false));
    assert_eq!(
        classify_attack_branch(&path.links, path.complete_below_slot, &attack),
        AttackBranch::None
    );
    assert!(proven_below_target_slot_path(attack.slot, attack.slot).is_none());
}

#[test]
fn canonical_branch_resolution_distinguishes_equivocation_candidates() {
    let attack = attack_evidence_for_branch_checks();
    let matching = |id| HeaderLink {
        id,
        parent: attack.parent_id,
        slot: attack.slot,
    };

    assert_eq!(
        classify_attack_branch(&[matching(attack.ordinary_id)], true, &attack),
        AttackBranch::Ordinary
    );
    assert_eq!(
        classify_attack_branch(&[matching(attack.siblings[2].1.id)], true, &attack),
        AttackBranch::Sibling(3)
    );
    assert_eq!(
        classify_attack_branch(&[matching(header_id(8))], true, &attack),
        AttackBranch::OtherCompeting(header_id(8))
    );
    assert_eq!(
        classify_attack_branch(&[], true, &attack),
        AttackBranch::None
    );
    assert_eq!(
        classify_attack_branch(&[], false, &attack),
        AttackBranch::Unverified
    );
}

#[test]
fn equal_tip_heights_do_not_imply_canonical_alignment() {
    assert!(canonical_tips_aligned(
        0,
        &[header_id(1), header_id(1), header_id(1)],
        1
    ));
    assert!(canonical_tips_aligned(
        1,
        &[header_id(1), header_id(1), header_id(1)],
        1
    ));
    assert!(!canonical_tips_aligned(
        0,
        &[header_id(1), header_id(2), header_id(1)],
        1
    ));
    assert!(!canonical_tips_aligned(
        2,
        &[header_id(1), header_id(1), header_id(1)],
        1
    ));
    assert!(!canonical_tips_aligned(0, &[], 1));
}

#[test]
fn tip_change_to_descendant_is_not_a_reorganization() {
    assert_eq!(
        classify_tip_transition(header_id(1), header_id(2), Some(true)),
        TipTransition::Descendant
    );
}

#[test]
fn tip_reorganization_above_same_attack_sibling_is_recorded_separately() {
    let attack_branch_before = AttackBranch::Sibling(1);
    let attack_branch_after = AttackBranch::Sibling(1);
    assert_eq!(attack_branch_before, attack_branch_after);
    assert_eq!(
        classify_tip_transition(header_id(7), header_id(8), Some(false)),
        TipTransition::Reorganization
    );
}

#[test]
fn missing_tip_ancestry_is_unverified_not_no_reorganization() {
    assert_eq!(
        classify_tip_transition(header_id(7), header_id(8), None),
        TipTransition::Unverified
    );
}

#[test]
fn quoted_receiver_failure_reason_preserves_spaces() {
    let fields = parse_log_fields(
        "2026-10-11T10:00:00.000Z INFO event=security_audit_proposal_reconstruction_failed reason=\"UnresolvedReference: transaction hash not in mempool\"",
    );

    assert_eq!(
        fields.get("reason").map(String::as_str),
        Some("UnresolvedReference: transaction hash not in mempool")
    );
}
