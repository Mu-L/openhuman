        true,
    )
    .unwrap();
    let too_new = create_flow(
        &config,
        "too-new".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();
    let newer_schema_json = serde_json::json!({
        "schema_version": 999,
        "name": "from-the-future",
        "nodes": [],
        "edges": []
    })
    .to_string();
    force_corrupt_graph_json_for_test(&config, &too_new.id, &newer_schema_json).unwrap();

    let (flows, skipped) = list_flows(&config).unwrap();
    assert_eq!(skipped, 1);
    assert_eq!(flows.len(), 1);
    assert_eq!(flows[0].id, good.id);
}

#[test]
fn list_enabled_flows_still_returns_the_good_rows_when_one_is_corrupt() {
    // This is the blast-radius scenario R-M4 flags for `bus.rs::handle_app_event`:
    // `list_enabled_flows` backs ALL `app_event` trigger dispatch, so one
    // corrupt enabled flow must not blackhole matching for every other one.
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);

    let good = create_flow(
        &config,
        "good".to_string(),
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
    force_corrupt_graph_json_for_test(&config, &bad.id, "not json at all").unwrap();

    let (enabled, skipped) = list_enabled_flows(&config).unwrap();
    assert_eq!(skipped, 1);
    assert_eq!(enabled.len(), 1);
    assert_eq!(enabled[0].id, good.id);
}

#[test]
fn list_enabled_flows_excludes_a_corrupt_disabled_row_without_counting_it_as_skipped() {
    // A corrupt row that was never enabled must not even be attempted for
    // decode by `list_enabled_flows` (the WHERE clause filters it out at the
    // SQL layer before `map_flow_row` ever runs) — it is neither returned nor
    // counted as skipped by this particular listing.
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);

    let good = create_flow(
        &config,
        "good".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();
    let disabled_and_corrupt = create_flow(
        &config,
        "disabled-bad".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();
    set_enabled(&config, &disabled_and_corrupt.id, false).unwrap();
    force_corrupt_graph_json_for_test(&config, &disabled_and_corrupt.id, "{{{").unwrap();

    let (enabled, skipped) = list_enabled_flows(&config).unwrap();
    assert_eq!(skipped, 0);
    assert_eq!(enabled.len(), 1);
    assert_eq!(enabled[0].id, good.id);
}

// ── R-m1: concurrent step upserts must not lose a step ──────────────────────

#[test]
fn concurrent_step_upserts_do_not_lose_a_step() {
    // Two observer callbacks for parallel branch nodes of the same run,
    // racing to persist their step. Before the `BEGIN IMMEDIATE` fix this was
    // a classic untransacted read-modify-write: both threads could read the
    // same pre-write `steps_json`, and whichever `UPDATE` landed last would
    // silently discard the other thread's step — permanently, since the
    // post-hoc `settle_steps` reconstruction only refills a missing node with
    // `status: None`, not its real outcome.
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
    let run_id = "run-concurrent";
    insert_flow_run(&config, run_id, &flow.id, run_id, "2026-01-01T00:00:00Z").unwrap();

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

    let config_a = config.clone();
    let barrier_a = barrier.clone();
    let handle_a = std::thread::spawn(move || {
        barrier_a.wait();
        upsert_flow_run_step(
            &config_a,
            run_id,
            &FlowRunStep {
                node_id: "branch-a".to_string(),
                output: serde_json::json!([{"json": {"a": 1}}]),
                status: Some("success".to_string()),
                ..Default::default()
            },
        )
    });

    let config_b = config.clone();
    let barrier_b = barrier.clone();
    let handle_b = std::thread::spawn(move || {
        barrier_b.wait();
        upsert_flow_run_step(
            &config_b,
            run_id,
            &FlowRunStep {
                node_id: "branch-b".to_string(),
                output: serde_json::json!([{"json": {"b": 1}}]),
                status: Some("success".to_string()),
                ..Default::default()
            },
        )
    });

    handle_a.join().unwrap().unwrap();
    handle_b.join().unwrap().unwrap();

    let row = get_flow_run(&config, run_id).unwrap().unwrap();
    let node_ids: std::collections::HashSet<&str> =
        row.steps.iter().map(|s| s.node_id.as_str()).collect();
    assert_eq!(
        row.steps.len(),
        2,
        "both concurrent steps must survive, none silently dropped: {:?}",
        row.steps
    );
    assert!(node_ids.contains("branch-a"));
    assert!(node_ids.contains("branch-b"));
}

#[test]
fn concurrent_upserts_to_the_same_node_id_do_not_corrupt_the_step_list() {
    // Same run, same node_id, racing "replace" writes — the transaction must
    // still leave exactly one entry for that node (whichever write wins the
    // serialization order), never a torn/duplicated list.
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
    let run_id = "run-same-node";
    insert_flow_run(&config, run_id, &flow.id, run_id, "2026-01-01T00:00:00Z").unwrap();

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut handles = Vec::new();
    for i in 0..2 {
        let config = config.clone();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            upsert_flow_run_step(
                &config,
                run_id,
                &FlowRunStep {
                    node_id: "same-node".to_string(),
                    output: serde_json::json!([{"json": {"attempt": i}}]),
                    status: Some("success".to_string()),
                    ..Default::default()
                },
            )
        }));
    }
    for h in handles {
        h.join().unwrap().unwrap();
    }

    let row = get_flow_run(&config, run_id).unwrap().unwrap();
    assert_eq!(
        row.steps.len(),
        1,
        "a re-upsert of the same node_id must replace, not duplicate: {:?}",
        row.steps
    );
    assert_eq!(row.steps[0].node_id, "same-node");
}

// ── R-m8: schema init is gated to once per process per database path ───────

#[test]
fn schema_initializes_correctly_on_a_fresh_database_and_is_idempotent_across_calls() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);

    // First-ever call against this database file in the process: exercises
    // the full schema DDL (CREATE TABLE batch + indexes) plus the
    // `require_approval` `add_column_if_missing` migration on a database that
    // has never been opened before.
    let flow = create_flow(
        &config,
        "fresh-db".to_string(),
        String::new(),
        trigger_graph(),
        true, // require_approval
        true,
    )
    .unwrap();
    assert!(
        flow.require_approval,
        "the post-hoc require_approval column must exist and be writable on a brand-new db"
    );

    // Repeat calls against the SAME path must not need (or re-run) DDL —
    // proves the cached "already initialized" state doesn't break ordinary
    // reads/writes on reuse.
    let (listed, skipped) = list_flows(&config).unwrap();
    assert_eq!(skipped, 0);
    assert_eq!(listed.len(), 1);
    assert!(listed[0].require_approval);

    let reloaded = get_flow(&config, &flow.id).unwrap().unwrap();
    assert!(reloaded.require_approval);

    let run_id = "run-schema-check";
    insert_flow_run(&config, run_id, &flow.id, run_id, "2026-01-01T00:00:00Z").unwrap();
    assert!(get_flow_run(&config, run_id).unwrap().is_some());
}

#[test]
fn schema_initializes_independently_for_each_distinct_database_path() {
    // Regression guard for the once-per-process cache: if it were keyed by a
    // single process-wide flag instead of by database path, opening a SECOND
    // independent workspace after the first would silently skip schema
    // creation and every write against it would fail with "no such table".
    let tmp_a = TempDir::new().unwrap();
    let config_a = test_config(&tmp_a);
    let flow_a = create_flow(
        &config_a,
        "a".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();

    let tmp_b = TempDir::new().unwrap();
    let config_b = test_config(&tmp_b);
    let flow_b = create_flow(
        &config_b,
        "b".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();

    assert_eq!(list_flows(&config_a).unwrap().0.len(), 1);
    assert_eq!(list_flows(&config_b).unwrap().0.len(), 1);
    assert_eq!(
        get_flow(&config_a, &flow_a.id).unwrap().unwrap().id,
        flow_a.id
    );
    assert_eq!(
        get_flow(&config_b, &flow_b.id).unwrap().unwrap().id,
        flow_b.id
    );
}

/// R-m8 regression: gating the DDL behind a per-path "already initialized" set
/// must not cost the store its self-healing.
///
/// Before the gate existed, the DDL ran on every `with_connection` call, so a
/// database deleted or replaced at runtime (workspace reset, manual deletion,
/// a restore) recovered on the very next call — `Connection::open` creates a
/// fresh empty file and `CREATE TABLE IF NOT EXISTS` repopulates it. With a
/// naive cache the set still reports "initialized" while the file behind it is
/// empty, and every query afterwards fails `no such table: flow_definitions`
/// until the process restarts. This pins the verify-on-hit that restores it.
#[test]
fn schema_reinitializes_when_the_database_file_is_deleted_at_runtime() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);

    // First use populates the per-path cache and creates the schema.
    let flow = create_flow(
        &config,
        "before-deletion".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();
    let (flows, _skipped) = list_flows(&config).unwrap();
    assert_eq!(flows.len(), 1, "sanity: the flow was persisted");

    // Simulate a workspace reset / manual deletion while the process lives on.
    let db_path = config.workspace_dir.join("flows").join("flows.db");
    assert!(
        db_path.exists(),
        "sanity: the flows db exists before deletion"
    );
    std::fs::remove_file(&db_path).unwrap();
    // WAL sidecars must go too, or SQLite can resurrect pages from them.
    let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _ = std::fs::remove_file(db_path.with_extension("db-shm"));

    // The cache still says this path is initialized. Without the verify-on-hit
    // this errors with `no such table: flow_definitions`.
    let (flows_after, skipped_after) = list_flows(&config)
        .expect("a deleted database must be re-initialized, not left wedged at 'no such table'");
    assert!(
        flows_after.is_empty(),
        "the recreated database starts empty — the prior flow is genuinely gone"
    );
    assert_eq!(skipped_after, 0, "an empty database skips nothing");

    // And the store is fully usable again, not merely readable.
    let recreated = create_flow(
        &config,
        "after-deletion".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .expect("writes must work against the re-initialized schema");
    assert_ne!(recreated.id, flow.id);
    let (flows_final, _) = list_flows(&config).unwrap();
    assert_eq!(flows_final.len(), 1);
}

// ── description: the field, and the upgrade path ──────────────────────────

#[test]
fn a_description_round_trips_through_the_store() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    let created = create_flow(
        &config,
        "Digest".to_string(),
        "Posts the weekly digest to Slack.".to_string(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();
    let read_back = get_flow(&config, &created.id).unwrap().unwrap();
    assert_eq!(read_back.description, "Posts the weekly digest to Slack.");
    // And through the list path, which uses a different SELECT.
    let (flows, skipped) = list_flows(&config).unwrap();
    assert_eq!(skipped, 0);
    assert_eq!(flows[0].description, "Posts the weekly digest to Slack.");
}

#[test]
fn a_database_written_before_the_column_existed_still_opens() {
    // The migration that matters. `add_column_if_missing` runs against a real
    // pre-existing `flows.db`, so this builds one WITHOUT the column — exactly
    // what an upgrading user has — and then opens it through the normal path.
    //
    // Constructed by hand rather than by checking in a fixture file: a binary
    // fixture would drift silently as the rest of the schema moves, and the
    // thing under test is one column, not the whole file format.
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);

    let db_path = tmp.path().join("workspace").join("flows").join("flows.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE flow_definitions (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL,
                graph_json  TEXT NOT NULL,
                enabled     INTEGER NOT NULL DEFAULT 1,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                last_run_at TEXT,
                last_status TEXT
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO flow_definitions
                (id, name, graph_json, enabled, created_at, updated_at)
             VALUES ('old-1', 'Legacy flow', ?1, 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            rusqlite::params![serde_json::to_string(&trigger_graph()).unwrap()],
        )
        .unwrap();
    }

    // Opening through the normal path must migrate, not fail.
    let flow = get_flow(&config, "old-1")
        .expect("an upgraded database must open")
        .expect("the pre-existing row must survive");
    assert_eq!(flow.name, "Legacy flow");
    // The row predates the column, so it reads back empty — which every
    // consumer already treats as "no description", not as corruption.
    assert_eq!(flow.description, "");
}

#[test]
fn an_update_without_a_description_leaves_the_stored_one_alone() {
    // The `COALESCE(?, description)` contract. An edit that only reshapes the
    // graph must not silently blank the catalogue line.
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    let created = create_flow(
        &config,
        "Digest".to_string(),
        "Posts the weekly digest.".to_string(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();

    let updated = update_flow_graph(
        &config,
        &created.id,
        "Digest renamed".to_string(),
        None,
        trigger_graph(),
        false,
        None,
        false,
        None,
    )
    .expect("update succeeds");
    assert_eq!(updated.name, "Digest renamed");
    assert_eq!(updated.description, "Posts the weekly digest.");
}

#[test]
fn an_update_can_replace_and_can_clear_the_description() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    let created = create_flow(
        &config,
        "Digest".to_string(),
        "Original.".to_string(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();

    let replaced = update_flow_graph(
        &config,
        &created.id,
        "Digest".to_string(),
        Some("Rewritten.".to_string()),
        trigger_graph(),
        false,
        None,
        false,
        None,
    )
    .unwrap();
    assert_eq!(replaced.description, "Rewritten.");

    // `Some("")` is the only way to say "clear it", and must work — otherwise
    // a bad description is unfixable through this path.
    let cleared = update_flow_graph(
        &config,
        &created.id,
        "Digest".to_string(),
        Some(String::new()),
        trigger_graph(),
        false,
        None,
        false,
        None,
    )
    .unwrap();
    assert_eq!(cleared.description, "");
}

#[test]
fn a_duplicate_carries_the_description_across() {
    // A duplicate is the same automation under a new name; its purpose is
    // unchanged, so an empty description on the copy would be a regression the
    // user has to repair by hand.
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    let source = create_flow(
        &config,
        "Digest".to_string(),
        "Posts the weekly digest.".to_string(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();
    let copy = insert_duplicate_flow(&config, &source, "Digest (copy)".to_string()).unwrap();
    assert_eq!(copy.description, "Posts the weekly digest.");
}
