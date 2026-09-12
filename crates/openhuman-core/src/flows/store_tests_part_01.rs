use super::*;
use crate::config::Config;
use tempfile::TempDir;
use tinyflows::model::{Node, NodeKind, WorkflowGraph};

fn test_config(tmp: &TempDir) -> Config {
    let config = Config {
        workspace_dir: tmp.path().join("workspace"),
        action_dir: tmp.path().join("workspace"),
        config_path: tmp.path().join("config.toml"),
        ..Config::default()
    };
    std::fs::create_dir_all(&config.workspace_dir).unwrap();
    config
}

fn trigger_graph() -> WorkflowGraph {
    WorkflowGraph {
        nodes: vec![Node {
            id: "t".to_string(),
            kind: NodeKind::Trigger,
            type_version: 1,
            name: "Trigger".to_string(),
            config: serde_json::Value::Null,
            ports: Vec::new(),
            position: None,
        }],
        ..Default::default()
    }
}

/// An automatic-trigger (`schedule`) graph — `trigger_is_automatic` returns
/// `true` for this, unlike [`trigger_graph`]'s manual (no `trigger_kind`)
/// trigger.
fn automatic_schedule_graph() -> WorkflowGraph {
    WorkflowGraph {
        nodes: vec![Node {
            id: "t".to_string(),
            kind: NodeKind::Trigger,
            type_version: 1,
            name: "Trigger".to_string(),
            config: serde_json::json!({ "trigger_kind": "schedule", "schedule": "0 9 * * *" }),
            ports: Vec::new(),
            position: None,
        }],
        ..Default::default()
    }
}

#[test]
fn create_get_list_delete_roundtrip() {
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
    assert_eq!(flow.name, "demo");
    assert!(flow.enabled);

    let fetched = get_flow(&config, &flow.id).unwrap().expect("flow present");
    assert_eq!(fetched.id, flow.id);
    assert_eq!(fetched.graph, flow.graph);

    let (listed, skipped) = list_flows(&config).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, flow.id);
    assert_eq!(skipped, 0);

    remove_flow(&config, &flow.id).unwrap();
    assert!(get_flow(&config, &flow.id).unwrap().is_none());
    assert!(list_flows(&config).unwrap().0.is_empty());
}

#[test]
fn get_flow_returns_none_for_unknown_id() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    assert!(get_flow(&config, "missing").unwrap().is_none());
}

#[test]
fn remove_flow_errors_when_not_found() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    let err = remove_flow(&config, "missing").unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[test]
fn set_enabled_toggles_and_persists() {
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
    assert!(flow.enabled);

    let disabled = set_enabled(&config, &flow.id, false).unwrap();
    assert!(!disabled.enabled);

    let reloaded = get_flow(&config, &flow.id).unwrap().unwrap();
    assert!(!reloaded.enabled);

    let enabled = set_enabled(&config, &flow.id, true).unwrap();
    assert!(enabled.enabled);
}

#[test]
fn update_flow_graph_bumps_updated_at_and_preserves_created_at() {
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

    let mut new_graph = trigger_graph();
    new_graph.name = "renamed-graph".to_string();
    let updated = update_flow_graph(
        &config,
        &flow.id,
        "renamed".to_string(),
        None,
        new_graph,
        false,
        None,
        false,
        None,
    )
    .unwrap();

    assert_eq!(updated.name, "renamed");
    assert_eq!(updated.created_at, flow.created_at);
    assert_eq!(updated.graph.name, "renamed-graph");
}

/// `enabled_override: None` must leave the persisted `enabled` column
/// exactly as it was — `update_flow_graph` re-reads the current row and
/// falls back to `current.enabled`, not to whatever the caller might have
/// observed earlier.
#[test]
fn update_flow_graph_with_none_override_preserves_current_enabled_column() {
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
    assert!(flow.enabled, "flow created enabled");

    let updated = update_flow_graph(
        &config,
        &flow.id,
        flow.name.clone(),
        None,
        trigger_graph(),
        false,
        None,  // enabled_override
        false, // force_disarm_if_automatic
        None,
    )
    .unwrap();

    assert!(
        updated.enabled,
        "a None override must preserve the row's current enabled state"
    );
    let reloaded = get_flow(&config, &flow.id).unwrap().unwrap();
    assert!(reloaded.enabled);
}

/// `enabled_override: Some(false)` must force-persist `enabled=false`
/// regardless of what the row's `enabled` column currently holds — this is
/// the mechanism `flows_update`'s B29 Rule 1 analogue relies on to disarm a
/// manual→automatic trigger transition in the same guarded write.
#[test]
fn update_flow_graph_with_some_false_override_forces_disabled() {
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
    assert!(flow.enabled, "flow created enabled");

    let updated = update_flow_graph(
        &config,
        &flow.id,
        flow.name.clone(),
        None,
        trigger_graph(),
        false,
        Some(false), // enabled_override
        false,       // force_disarm_if_automatic
        None,
    )
    .unwrap();

    assert!(
        !updated.enabled,
        "a Some(false) override must force enabled=false even though the row was enabled"
    );
    let reloaded = get_flow(&config, &flow.id).unwrap().unwrap();
    assert!(!reloaded.enabled);
}

/// Regression for the silent live-arming race Codex flagged on this PR:
/// `flows_update` (ops.rs) makes its manual→automatic disarm decision from
/// an *outer* `existing` read taken before `update_flow_graph`'s own guarded
/// UPDATE re-reads the row. If a concurrent `flows_set_enabled(id, true)`
/// landed in that gap — which bumps `updated_at`, so it would NOT trip the
/// optimistic-concurrency conflict — the outer read would be stale while the
/// row is actually enabled by write time. This proves the mechanism the fix
/// relies on to close that race: an `enabled_override` of `Some(false)`
/// (what `flows_update` now passes unconditionally on a manual→automatic
/// transition, never gated on the stale outer read) always wins over
/// whatever the row's `enabled` column was concurrently flipped to,
/// simulated here by flipping it with `set_enabled` between the two calls.
#[test]
fn update_flow_graph_override_wins_over_concurrently_enabled_row() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    let flow = create_flow(
        &config,
        "demo".to_string(),
        String::new(),
        trigger_graph(),
        false,
        false,
    )
    .unwrap();
    assert!(!flow.enabled, "flow created disabled");

    // Simulates a concurrent `flows_set_enabled(id, true)` racing in after
    // `flows_update`'s outer `existing` read observed `enabled: false`, but
    // before its guarded `update_flow_graph` write below.
    let raced = set_enabled(&config, &flow.id, true).unwrap();
    assert!(raced.enabled);

    let updated = update_flow_graph(
        &config,
        &flow.id,
        flow.name.clone(),
        None,
        trigger_graph(),
        false,
        Some(false), // the unconditional disarm override
        false,       // force_disarm_if_automatic
        None,
    )
    .unwrap();

    assert!(
        !updated.enabled,
        "the disarm override must win over a concurrently-enabled row, not the reverse"
    );
    let reloaded = get_flow(&config, &flow.id).unwrap().unwrap();
    assert!(!reloaded.enabled);
}

/// R-m2 regression: the manual→automatic disarm decision must be computed
/// against the row `update_flow_graph` JUST re-read (`current`), never a
/// caller-supplied belief about the flow's prior state. Before the fix,
/// `ops::flows_update` computed this transition from an OUTER `existing`
/// read taken before calling into the store — a concurrent write between
/// that read and this call could make the transition invisible to the
/// caller, letting an automatic-trigger graph persist `enabled: true`.
///
/// Proven here without needing to fake a race: the disarm must fire from
/// `current.graph` (MANUAL) vs the new `graph` (automatic) alone, and must
/// WIN over an `enabled_override` that explicitly asks to stay enabled —
/// exactly the shape of override a stale caller-side decision could
/// otherwise have smuggled through.
#[test]
fn update_flow_graph_disarms_transition_from_the_fresh_row_even_when_override_asks_to_stay_enabled()
{
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
    assert!(flow.enabled, "flow created enabled");

    let updated = update_flow_graph(
        &config,
        &flow.id,
        flow.name.clone(),
        None,
        automatic_schedule_graph(),
        false,
        Some(true), // caller explicitly asks to stay enabled
        false,      // force_disarm_if_automatic (the remote-authoring flag) OFF —
        // proving the unconditional Rule 1 transition-disarm fires on its own
        None,
    )
    .unwrap();

    assert!(
        !updated.enabled,
        "a manual->automatic transition must disarm even when enabled_override asks to stay \
         enabled — the disarm always wins (R-m2)"
    );
    let reloaded = get_flow(&config, &flow.id).unwrap().unwrap();
    assert!(!reloaded.enabled);
}

/// Sibling of the above: when there is NO transition (the row was already
/// automatic before this call, matching what's actually in the DB right
/// now), an ordinary `enabled_override` is honoured normally — the fix must
/// not over-disarm every automatic-trigger update, only genuine
/// manual/none → automatic transitions (unless `force_disarm_if_automatic`
/// is also set).
#[test]
fn update_flow_graph_does_not_disarm_an_automatic_to_automatic_update() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    let flow = create_flow(
        &config,
        "demo".to_string(),
        String::new(),
        automatic_schedule_graph(),
        false,
        false,
    )
    .unwrap();
    assert!(!flow.enabled, "born disabled — armed explicitly next");
    let armed = set_enabled(&config, &flow.id, true).unwrap();
    assert!(armed.enabled);

    let updated = update_flow_graph(
        &config,
        &flow.id,
        flow.name.clone(),
        None,
        automatic_schedule_graph(),
        false,
        None,  // no explicit override — preserve current.enabled
        false, // force_disarm_if_automatic OFF
        None,
    )
    .unwrap();

    assert!(
        updated.enabled,
        "an automatic->automatic update (no transition) must not be auto-disarmed"
    );
}

#[test]
fn record_run_sets_last_run_fields() {
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
    assert!(flow.last_run_at.is_none());

    record_run(&config, &flow.id, "completed").unwrap();
    let reloaded = get_flow(&config, &flow.id).unwrap().unwrap();
    assert!(reloaded.last_run_at.is_some());
    assert_eq!(reloaded.last_status.as_deref(), Some("completed"));
}

#[test]
fn stored_graph_older_than_current_schema_is_migrated_on_read() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);

    // Insert a raw, versionless graph row directly (bypassing create_flow's
    // typed path) to simulate a definition persisted by an older crate build.
    let legacy_graph_json = serde_json::json!({
        "name": "legacy",
        "nodes": [{ "id": "t", "kind": "trigger", "name": "Trigger" }],
        "edges": []
    })
    .to_string();

    with_connection(&config, |conn| {
        conn.execute(
            "INSERT INTO flow_definitions
                (id, name, graph_json, enabled, created_at, updated_at, last_run_at, last_status)
             VALUES ('legacy-1', 'legacy', ?1, 1, '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z', NULL, NULL)",
            rusqlite::params![legacy_graph_json],
        )?;
        Ok(())
    })
    .unwrap();

    let loaded = get_flow(&config, "legacy-1").unwrap().expect("row present");
    assert_eq!(
        loaded.graph.schema_version,
        tinyflows::model::CURRENT_SCHEMA_VERSION
    );
    assert_eq!(loaded.graph.nodes.len(), 1);
}

#[test]
fn kv_get_set_round_trips_and_is_namespace_scoped() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);

    assert!(kv_get(&config, "ns1", "k").unwrap().is_none());

    kv_set(&config, "ns1", "k", &serde_json::json!({"v": 1})).unwrap();
    assert_eq!(
        kv_get(&config, "ns1", "k").unwrap(),
        Some(serde_json::json!({"v": 1}))
    );

    // A different namespace does not see ns1's value.
    assert!(kv_get(&config, "ns2", "k").unwrap().is_none());

    // Overwrite.
    kv_set(&config, "ns1", "k", &serde_json::json!(2)).unwrap();
    assert_eq!(
        kv_get(&config, "ns1", "k").unwrap(),
        Some(serde_json::json!(2))
    );
}

// ── require_approval ─────────────────────────────────────────────────────

#[test]
fn create_flow_persists_require_approval() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);

    let flow = create_flow(
        &config,
        "demo".to_string(),
        String::new(),
        trigger_graph(),
        true,
        true,
    )
    .unwrap();
    assert!(flow.require_approval);

    let reloaded = get_flow(&config, &flow.id).unwrap().unwrap();
    assert!(reloaded.require_approval);
}

#[test]
fn update_flow_graph_can_change_require_approval() {
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
    assert!(!flow.require_approval);

    let updated = update_flow_graph(
        &config,
        &flow.id,
        flow.name.clone(),
        None,
        trigger_graph(),
        true,
        None,
        false,
        None,
    )
    .unwrap();
    assert!(updated.require_approval);

    let reloaded = get_flow(&config, &flow.id).unwrap().unwrap();
    assert!(reloaded.require_approval);
}

#[test]
fn legacy_flow_definitions_row_without_require_approval_column_defaults_false() {
    // A row inserted before the `require_approval` column existed. Schema
    // init (including the `add_column_if_missing` ALTER) runs once per
    // process per database file (R-m8) — since this test opens a fresh
    // per-`TempDir` database, that one-time init still runs here, simulating
    // a workspace opened once on an older build.
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);

    let legacy_graph_json = serde_json::to_string(&trigger_graph()).unwrap();
    with_connection(&config, |conn| {
        conn.execute(
            "INSERT INTO flow_definitions
                (id, name, graph_json, enabled, created_at, updated_at, last_run_at, last_status)
             VALUES ('legacy-2', 'legacy', ?1, 1, '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z', NULL, NULL)",
            rusqlite::params![legacy_graph_json],
        )?;
        Ok(())
    })
    .unwrap();

    let loaded = get_flow(&config, "legacy-2").unwrap().expect("row present");
    assert!(!loaded.require_approval);
}

// ── list_enabled_flows ────────────────────────────────────────────────────

#[test]
fn list_enabled_flows_excludes_disabled() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);

    let enabled_flow = create_flow(
        &config,
        "enabled".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();
    let disabled_flow = create_flow(
        &config,
        "disabled".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();
    set_enabled(&config, &disabled_flow.id, false).unwrap();

    let (enabled, skipped) = list_enabled_flows(&config).unwrap();
    assert_eq!(enabled.len(), 1);
    assert_eq!(enabled[0].id, enabled_flow.id);
    assert_eq!(skipped, 0);
}

// ── flow_runs CRUD ────────────────────────────────────────────────────────

#[test]
fn flow_run_insert_finish_get_round_trip() {
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

    let thread_id = format!("flow:{}:run-1", flow.id);
    insert_flow_run(
        &config,
        &thread_id,
        &flow.id,
        &thread_id,
        "2026-01-01T00:00:00Z",
    )
    .unwrap();

    let running = get_flow_run(&config, &thread_id)
        .unwrap()
        .expect("row present");
    assert_eq!(running.status, "running");
    assert!(running.finished_at.is_none());
    assert!(running.steps.is_empty());

    let steps = vec![FlowRunStep {
        node_id: "t".to_string(),
        output: serde_json::json!([{"json": {"x": 1}}]),
        port: None,
        ..Default::default()
    }];
    finish_flow_run(
        &config,
        &thread_id,
        "completed",
        "2026-01-01T00:00:01Z",
        &steps,
        &[],
        None,
        None,
    )
    .unwrap();

    let finished = get_flow_run(&config, &thread_id)
        .unwrap()
        .expect("row present");
    assert_eq!(finished.status, "completed");
    assert_eq!(
        finished.finished_at.as_deref(),
        Some("2026-01-01T00:00:01Z")
    );
    assert_eq!(finished.steps.len(), 1);
    assert_eq!(finished.steps[0].node_id, "t");
    assert!(finished.pending_approvals.is_empty());
    assert!(finished.error.is_none());
}

#[test]
fn finish_flow_run_records_error_on_failure() {
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
    let thread_id = format!("flow:{}:run-2", flow.id);
    insert_flow_run(
        &config,
        &thread_id,
        &flow.id,
        &thread_id,
        "2026-01-01T00:00:00Z",
    )
    .unwrap();

    finish_flow_run(
        &config,
        &thread_id,
        "failed",
        "2026-01-01T00:00:01Z",
        &[],
        &[],
        Some("boom"),
        None,
    )
    .unwrap();

    let finished = get_flow_run(&config, &thread_id).unwrap().unwrap();
    assert_eq!(finished.status, "failed");
    assert_eq!(finished.error.as_deref(), Some("boom"));
}

#[test]
fn get_flow_run_returns_none_for_unknown_id() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    assert!(get_flow_run(&config, "missing").unwrap().is_none());
}

#[test]
fn list_flow_runs_orders_newest_first_and_is_scoped_to_flow() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    let flow_a = create_flow(
        &config,
        "a".to_string(),
        String::new(),
        trigger_graph(),
        false,
        true,
    )
    .unwrap();
    let flow_b = create_flow(
