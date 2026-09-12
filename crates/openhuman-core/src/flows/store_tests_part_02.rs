        &config,
        "b".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();

    insert_flow_run(
        &config,
        "run-a1",
        &flow_a.id,
        "run-a1",
        "2026-01-01T00:00:00Z",
    )
    .unwrap();
    insert_flow_run(
        &config,
        "run-a2",
        &flow_a.id,
        "run-a2",
        "2026-01-02T00:00:00Z",
    )
    .unwrap();
    insert_flow_run(
        &config,
        "run-b1",
        &flow_b.id,
        "run-b1",
        "2026-01-01T00:00:00Z",
    )
    .unwrap();

    let runs_a = list_flow_runs(&config, &flow_a.id, 10).unwrap();
    assert_eq!(runs_a.len(), 2);
    assert_eq!(runs_a[0].id, "run-a2", "newest run must come first");
    assert_eq!(runs_a[1].id, "run-a1");

    let runs_b = list_flow_runs(&config, &flow_b.id, 10).unwrap();
    assert_eq!(runs_b.len(), 1);
    assert_eq!(runs_b[0].id, "run-b1");
}

// ── insert_duplicate_flow ─────────────────────────────────────────────────

#[test]
fn insert_duplicate_flow_makes_a_disabled_copy_with_new_id_and_same_graph() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);

    // Enabled source with require_approval + a distinctive graph name.
    let mut graph = trigger_graph();
    graph.name = "original-graph".to_string();
    let source = create_flow(
        &config,
        "My Flow".to_string(),
        String::new(),
        graph,
        true,
        true,
    )
    .unwrap();
    assert!(source.enabled);
    record_run(&config, &source.id, "completed").unwrap();
    let source = get_flow(&config, &source.id).unwrap().unwrap();
    assert!(source.last_status.is_some());

    let copy = insert_duplicate_flow(&config, &source, "My Flow (copy)".to_string()).unwrap();

    // New id, suffixed name, DISABLED, run history reset.
    assert_ne!(copy.id, source.id);
    assert_eq!(copy.name, "My Flow (copy)");
    assert!(
        !copy.enabled,
        "duplicate must be disabled so it never fires"
    );
    assert!(copy.last_run_at.is_none());
    assert!(copy.last_status.is_none());
    // Same graph + require_approval carried over.
    assert_eq!(copy.graph, source.graph);
    assert_eq!(copy.graph.name, "original-graph");
    assert!(copy.require_approval);

    // Persisted and independent — both rows exist.
    let reloaded = get_flow(&config, &copy.id).unwrap().unwrap();
    assert!(!reloaded.enabled);
    assert_eq!(reloaded.graph, source.graph);
    assert_eq!(list_flows(&config).unwrap().0.len(), 2);
}

// ── prune_flow_runs ───────────────────────────────────────────────────────

fn seed_run(config: &Config, flow_id: &str, id: &str, day: u32, status: &str) {
    let started = format!("2026-01-{day:02}T00:00:00Z");
    insert_flow_run(config, id, flow_id, id, &started).unwrap();
    if status != "running" {
        finish_flow_run(
            config,
            id,
            status,
            &format!("2026-01-{day:02}T00:00:05Z"),
            &[],
            &[],
            None,
            None,
        )
        .unwrap();
    }
}

#[test]
fn prune_flow_runs_keeps_newest_n_terminal_runs() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    let flow = create_flow(
        &config,
        "demo".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();

    // 5 completed runs on ascending days.
    for i in 1..=5 {
        seed_run(&config, &flow.id, &format!("run-{i}"), i, "completed");
    }

    let deleted = prune_flow_runs(&config, &flow.id, 2).unwrap();
    assert_eq!(deleted, 3, "5 terminal runs, keep 2 => 3 pruned");

    let remaining = list_flow_runs(&config, &flow.id, 100).unwrap();
    let ids: Vec<_> = remaining.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["run-5", "run-4"], "newest two survive");
}

#[test]
fn prune_flow_runs_never_removes_pending_approval_run() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    let flow = create_flow(
        &config,
        "demo".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();

    // An OLD parked pending_approval run (day 1) plus newer completed runs.
    seed_run(&config, &flow.id, "parked", 1, "pending_approval");
    for i in 2..=5 {
        seed_run(&config, &flow.id, &format!("run-{i}"), i, "completed");
    }

    // keep=1 would normally leave only the newest run; the parked one must
    // still survive despite being the oldest and outside the newest-1 window.
    let deleted = prune_flow_runs(&config, &flow.id, 1).unwrap();
    let remaining = list_flow_runs(&config, &flow.id, 100).unwrap();
    let ids: std::collections::HashSet<_> = remaining.iter().map(|r| r.id.as_str()).collect();
    assert!(
        ids.contains("parked"),
        "a pending_approval run must never be pruned out from under a resume"
    );
    assert!(ids.contains("run-5"), "newest terminal run kept");
    // Only terminal runs 2..4 were eligible; 5 kept by window => 3 deleted.
    assert_eq!(deleted, 3);
}

#[test]
fn prune_flow_runs_leaves_running_rows_alone() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    let flow = create_flow(
        &config,
        "demo".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();

    seed_run(&config, &flow.id, "live", 1, "running");
    for i in 2..=4 {
        seed_run(&config, &flow.id, &format!("run-{i}"), i, "completed");
    }

    prune_flow_runs(&config, &flow.id, 1).unwrap();
    let remaining = list_flow_runs(&config, &flow.id, 100).unwrap();
    let ids: std::collections::HashSet<_> = remaining.iter().map(|r| r.id.as_str()).collect();
    assert!(ids.contains("live"), "a running run is never pruned");
}

#[test]
fn insert_flow_run_auto_prunes_beyond_retention_cap() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    let flow = create_flow(
        &config,
        "demo".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();

    // Seed exactly MAX_FLOW_RUNS_PER_FLOW completed runs.
    let cap = MAX_FLOW_RUNS_PER_FLOW;
    for i in 0..cap {
        let id = format!("run-{i:04}");
        insert_flow_run(
            &config,
            &id,
            &flow.id,
            &id,
            &format!("2026-01-01T00:00:{i:02}Z"),
        )
        .unwrap();
        finish_flow_run(
            &config,
            &id,
            "completed",
            "2026-01-01T00:01:00Z",
            &[],
            &[],
            None,
            None,
        )
        .unwrap();
    }
    assert_eq!(
        list_flow_runs(&config, &flow.id, cap * 2).unwrap().len(),
        cap
    );

    // One more insert should trigger the retention prune, keeping <= cap.
    let extra = "run-extra";
    insert_flow_run(&config, extra, &flow.id, extra, "2026-01-02T00:00:00Z").unwrap();
    let count = list_flow_runs(&config, &flow.id, cap * 2).unwrap().len();
    assert!(
        count <= cap,
        "auto-prune should keep run count within cap ({count} > {cap})"
    );
}

#[test]
fn list_flow_runs_respects_limit() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    let flow = create_flow(
        &config,
        "demo".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();

    for i in 0..3 {
        let id = format!("run-{i}");
        insert_flow_run(
            &config,
            &id,
            &flow.id,
            &id,
            &format!("2026-01-0{}T00:00:00Z", i + 1),
        )
        .unwrap();
    }

    let limited = list_flow_runs(&config, &flow.id, 2).unwrap();
    assert_eq!(limited.len(), 2);
}

// ── flow_suggestions ─────────────────────────────────────────────────────────

fn sample_suggestion(id: &str, title: &str) -> FlowSuggestion {
    FlowSuggestion {
        id: id.to_string(),
        title: title.to_string(),
        one_liner: "does a useful thing".to_string(),
        rationale: "grounded in your data".to_string(),
        trigger_hint: Some("schedule".to_string()),
        steps_outline: vec!["step one".to_string(), "step two".to_string()],
        suggested_connections: vec!["composio:gmail:conn_1".to_string()],
        suggested_slugs: vec!["GMAIL_SEND_EMAIL".to_string()],
        build_prompt: "Build a workflow that…".to_string(),
        confidence: 0.7,
        status: SuggestionStatus::New,
        created_at: "2026-07-05T00:00:00Z".to_string(),
        source_run_id: Some("run-1".to_string()),
    }
}

#[test]
fn suggestions_upsert_list_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);

    let written = upsert_suggestions(
        &config,
        &[
            sample_suggestion("s1", "Alpha"),
            sample_suggestion("s2", "Beta"),
        ],
    )
    .unwrap();
    assert_eq!(written, 2);

    let all = list_suggestions(&config, Some(SuggestionStatus::New), 50).unwrap();
    assert_eq!(all.len(), 2);
    // Round-trips the JSON-encoded vec columns.
    let alpha = all.iter().find(|s| s.id == "s1").unwrap();
    assert_eq!(alpha.steps_outline.len(), 2);
    assert_eq!(alpha.suggested_connections, vec!["composio:gmail:conn_1"]);
    assert_eq!(alpha.suggested_slugs, vec!["GMAIL_SEND_EMAIL"]);
    assert_eq!(alpha.trigger_hint.as_deref(), Some("schedule"));
}

#[test]
fn upsert_suggestions_preserves_user_status_on_rerun() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);

    upsert_suggestions(&config, &[sample_suggestion("s1", "Alpha")]).unwrap();
    // User dismisses it.
    assert!(set_suggestion_status(&config, "s1", SuggestionStatus::Dismissed).unwrap());

    // A later discovery run re-proposes the identical idea (same id) with a
    // refreshed pitch — the dismissal must survive.
    let mut refreshed = sample_suggestion("s1", "Alpha (refined)");
    refreshed.status = SuggestionStatus::New; // agent always emits `New`
    upsert_suggestions(&config, &[refreshed]).unwrap();

    let dismissed = list_suggestions(&config, Some(SuggestionStatus::Dismissed), 50).unwrap();
    assert_eq!(dismissed.len(), 1);
    assert_eq!(dismissed[0].title, "Alpha (refined)"); // pitch fields refreshed
                                                       // …but it is NOT back in the active `New` list.
    let active = list_suggestions(&config, Some(SuggestionStatus::New), 50).unwrap();
    assert!(active.is_empty());
}

#[test]
fn set_suggestion_status_returns_false_for_unknown_id() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    assert!(!set_suggestion_status(&config, "missing", SuggestionStatus::Built).unwrap());
}

#[test]
fn list_suggestions_without_status_returns_all() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    upsert_suggestions(&config, &[sample_suggestion("s1", "Alpha")]).unwrap();
    set_suggestion_status(&config, "s1", SuggestionStatus::Built).unwrap();
    // Filtered to `New` → empty; unfiltered → present.
    assert!(list_suggestions(&config, Some(SuggestionStatus::New), 50)
        .unwrap()
        .is_empty());
    assert_eq!(list_suggestions(&config, None, 50).unwrap().len(), 1);
}

#[test]
fn upsert_suggestions_empty_is_noop() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    assert_eq!(upsert_suggestions(&config, &[]).unwrap(), 0);
}

// ── Orphaned-running-run reconciliation (bug B42) ──────────────────────────

#[test]
fn list_running_run_ids_returns_only_running_rows() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    let flow = create_flow(
        &config,
        "demo".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();

    insert_flow_run(
        &config,
        "run-live-1",
        &flow.id,
        "run-live-1",
        "2026-01-01T00:00:00Z",
    )
    .unwrap();
    insert_flow_run(
        &config,
        "run-live-2",
        &flow.id,
        "run-live-2",
        "2026-01-01T00:00:01Z",
    )
    .unwrap();
    insert_flow_run(
        &config,
        "run-done",
        &flow.id,
        "run-done",
        "2026-01-01T00:00:02Z",
    )
    .unwrap();
    finish_flow_run(
        &config,
        "run-done",
        "completed",
        "2026-01-01T00:00:03Z",
        &[],
        &[],
        None,
        None,
    )
    .unwrap();

    let mut running = list_running_run_ids(&config, "2099-01-01T00:00:00Z").unwrap();
    running.sort();
    assert_eq!(
        running,
        vec![
            ("run-live-1".to_string(), flow.id.clone()),
            ("run-live-2".to_string(), flow.id.clone()),
        ],
        "only the two still-running rows must be listed, not the completed one"
    );
}

#[test]
fn list_running_run_ids_excludes_rows_started_at_or_after_the_floor() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    let flow = create_flow(
        &config,
        "demo".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();

    insert_flow_run(
        &config,
        "run-old",
        &flow.id,
        "run-old",
        "2026-01-01T00:00:00Z",
    )
    .unwrap();
    insert_flow_run(
        &config,
        "run-at",
        &flow.id,
        "run-at",
        "2026-01-01T00:00:05Z",
    )
    .unwrap();
    insert_flow_run(
        &config,
        "run-new",
        &flow.id,
        "run-new",
        "2026-01-01T00:00:09Z",
    )
    .unwrap();

    // The floor is exclusive: a row stamped exactly at the boot floor was
    // inserted by THIS process (`start_flow_run_row` anchors the floor before
    // stamping), so it must fall outside the candidate set along with newer
    // rows — otherwise the sweep could interrupt a live run and drop its
    // checkpoint mid-flight.
    let running = list_running_run_ids(&config, "2026-01-01T00:00:05Z").unwrap();
    assert_eq!(
        running,
        vec![("run-old".to_string(), flow.id.clone())],
        "only rows strictly older than the floor are sweep candidates"
    );
}

#[test]
fn mark_run_interrupted_reconciles_a_running_row_with_reason() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    let flow = create_flow(
        &config,
        "demo".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();
    insert_flow_run(&config, "run-x", &flow.id, "run-x", "2026-01-01T00:00:00Z").unwrap();

    let flipped =
        mark_run_interrupted(&config, "run-x", "2026-01-01T00:05:00Z", "boom reason").unwrap();
    assert!(flipped, "a running row must be reconciled");

    let row = get_flow_run(&config, "run-x").unwrap().unwrap();
    assert_eq!(row.status, "interrupted");
    assert_eq!(row.finished_at.as_deref(), Some("2026-01-01T00:05:00Z"));
    assert_eq!(row.error.as_deref(), Some("boom reason"));
}

#[test]
fn mark_run_interrupted_is_a_noop_for_a_terminal_row() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    let flow = create_flow(
        &config,
        "demo".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();
    insert_flow_run(&config, "run-y", &flow.id, "run-y", "2026-01-01T00:00:00Z").unwrap();
    finish_flow_run(
        &config,
        "run-y",
        "completed",
        "2026-01-01T00:00:01Z",
        &[],
        &[],
        None,
        None,
    )
    .unwrap();

    // The `status = 'running'` guard must protect an already-settled run.
    let flipped =
        mark_run_interrupted(&config, "run-y", "2026-01-01T00:05:00Z", "should not apply").unwrap();
    assert!(
        !flipped,
        "a completed run must never be clobbered to interrupted"
    );

    let row = get_flow_run(&config, "run-y").unwrap().unwrap();
    assert_eq!(row.status, "completed");
    assert!(row.error.is_none());
}

/// `expire_parked_runs` must return only the runs it ACTUALLY flipped, not the
/// candidates its `SELECT` saw.
///
/// The `SELECT` and each row's guarded `UPDATE` are separate statements on an
/// autocommit connection, so a concurrent `mark_run_resuming` can claim a row in
/// between. The per-row `WHERE status = 'pending_approval'` keeps that row safe,
/// but returning the unfiltered candidate list would let the caller act on a run
/// it never expired — dropping the checkpoint out from under a live resume and
/// publishing a terminal `FlowRunFinished` for a run still executing. That false
/// event is the worse half: the frontend de-dupes terminal events per
/// `flow_id:run_id`, so the run's real completion would later be discarded.
#[test]
fn expire_parked_runs_returns_only_rows_it_actually_flipped() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    let flow = create_flow(
        &config,
        "ttl".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();

    let stale_at = "2000-01-01T00:00:00+00:00";
    for id in ["claimed-run", "genuinely-stale-run"] {
        insert_flow_run(&config, id, &flow.id, id, stale_at).unwrap();
        finish_flow_run(
            &config,
            id,
            "pending_approval",
            stale_at,
            &[],
            &["gate".to_string()],
            None,
            // No graph pin (T-M1): this fixture is about the TTL sweep's
            // candidates-vs-sweeps behaviour, not stale-approval detection, so
            // these rows stand in for pre-pin legacy parks.
            None,
        )
        .unwrap();
    }

    // Simulate the race: one candidate is claimed by a resume after the sweep's
    // SELECT would have seen it, but before its UPDATE lands.
    assert!(mark_run_resuming(&config, "claimed-run").unwrap());

    let swept = expire_parked_runs(
        &config,
        "2099-01-01T00:00:00+00:00",
        "2026-01-01T00:00:00+00:00",
        "expired",
    )
    .unwrap();

    let swept_ids: Vec<&str> = swept.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        swept_ids,
        vec!["genuinely-stale-run"],
        "only the row whose guarded UPDATE matched may be reported as swept"
    );
    assert_eq!(
        get_flow_run(&config, "claimed-run")
            .unwrap()
            .unwrap()
            .status,
        "running",
        "the claimed run must keep executing, untouched by the sweep"
    );
    assert_eq!(
        get_flow_run(&config, "genuinely-stale-run")
            .unwrap()
            .unwrap()
            .status,
        "cancelled"
    );
}

// ── R-M4: corrupt/unmigratable graph_json rows must not brick a list ────────

#[test]
fn list_flows_skips_a_corrupt_row_and_reports_the_count() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);

    let good_a = create_flow(
        &config,
        "good-a".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();
    let bad = create_flow(
        &config,
        "bad".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();
    let good_b = create_flow(
        &config,
        "good-b".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();
    force_corrupt_graph_json_for_test(&config, &bad.id, "{ not even valid json").unwrap();

    let (flows, skipped) = list_flows(&config).unwrap();
    assert_eq!(
        skipped, 1,
        "exactly the one corrupt row must be counted as skipped"
    );
    let ids: Vec<&str> = flows.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(
        flows.len(),
        2,
        "the two good rows must still be returned: {ids:?}"
    );
    assert!(ids.contains(&good_a.id.as_str()));
    assert!(ids.contains(&good_b.id.as_str()));
    assert!(!ids.contains(&bad.id.as_str()));
}

#[test]
fn list_flows_skips_a_row_whose_schema_version_is_newer_than_this_build_supports() {
    // The real-world R-M4 scenario: a user ran a newer build that persisted a
    // graph at a `schema_version` this build's `tinyflows::migrate::migrate`
    // cannot step backward from, then downgraded.
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);

    let good = create_flow(
        &config,
        "good".to_string(),
        String::new(),
        trigger_graph(),
        false,
