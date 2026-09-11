
    fn open_test_conn() -> (NamedTempFile, Connection) {
        let f = NamedTempFile::new().unwrap();
        let conn = Connection::open(f.path()).unwrap();
        init_schema(&conn).unwrap();
        (f, conn)
    }

    /// A `Config` pointing at a throwaway workspace, for the helpers that open
    /// their own connection via `with_connection`.
    fn test_config() -> (tempfile::TempDir, Config) {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.workspace_dir = dir.path().to_path_buf();
        (dir, config)
    }

    /// The RMW closure must see the record as it stands in the DB *now*, read
    /// inside the transaction — not a snapshot the caller captured earlier. A
    /// stale `transport` would let the credential-scope check mis-classify and
    /// carry a concurrent edit's credentials across origins.
    #[test]
    fn rmw_passes_the_current_persisted_record_to_the_closure() {
        let (_dir, config) = test_config();
        let mut server = sample_server("srv-rmw");
        server.provenance = ServerProvenance::Custom;
        server.transport = Transport::Stdio;
        insert_custom_server_with_env(&config, &server, &HashMap::new()).unwrap();

        let seen_transport = std::cell::Cell::new("");
        update_custom_server_rmw(&config, "srv-rmw", |current, _stored| {
            // Read from the DB inside the txn, so it reflects the persisted Stdio.
            seen_transport.set(current.transport.dispatch_kind());
            let mut updated = current.clone();
            updated.transport = Transport::HttpRemote {
                url: "https://x.io/mcp".to_string(),
            };
            Ok((updated, HashMap::new()))
        })
        .unwrap();

        assert_eq!(seen_transport.get(), "stdio", "closure saw the DB record");
        assert_eq!(
            get_server(&config, "srv-rmw")
                .unwrap()
                .transport
                .dispatch_kind(),
            "http_remote",
            "the write landed"
        );
    }

    /// A record removed before the RMW acquires its lock errors instead of
    /// committing a phantom write.
    #[test]
    fn rmw_errors_when_the_row_is_gone() {
        let (_dir, config) = test_config();
        let err = update_custom_server_rmw(&config, "nope", |current, stored| {
            Ok((current.clone(), stored))
        })
        .expect_err("missing row must error");
        assert!(err.to_string().contains("not found"), "got: {err}");
    }

    fn sample_server(id: &str) -> InstalledServer {
        InstalledServer {
            server_id: id.to_string(),
            qualified_name: "@test/server".to_string(),
            display_name: "Test Server".to_string(),
            description: Some("A test server".to_string()),
            icon_url: None,
            command_kind: CommandKind::Node,
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "@test/server".to_string()],
            env_keys: vec!["API_KEY".to_string()],
            config: None,
            installed_at: 1_700_000_000_000,
            last_connected_at: None,
            transport: Transport::Stdio,
            enabled: true,
            provenance: ServerProvenance::Registry,
        }
    }

    fn sample_http_server(id: &str, url: &str) -> InstalledServer {
        InstalledServer {
            server_id: id.to_string(),
            qualified_name: "@test/http-server".to_string(),
            display_name: "Test HTTP Server".to_string(),
            description: None,
            icon_url: None,
            command_kind: CommandKind::Node, // unused for HTTP
            command: String::new(),
            args: Vec::new(),
            env_keys: Vec::new(),
            config: None,
            installed_at: 1_700_000_000_000,
            last_connected_at: None,
            transport: Transport::HttpRemote {
                url: url.to_string(),
            },
            enabled: true,
            provenance: ServerProvenance::Registry,
        }
    }

    /// A hand-added server persists `provenance = 'custom'` and reads back as such,
    /// which is what keeps `update_custom` from editing a catalog install.
    #[test]
    fn custom_provenance_round_trips() {
        let (_f, conn) = open_test_conn();
        let mut server = sample_server("srv-custom");
        server.qualified_name = "custom/my-server".to_string();
        server.provenance = ServerProvenance::Custom;
        insert_server_conn(&conn, &server).unwrap();
        let loaded = get_server_conn(&conn, "srv-custom").unwrap();
        assert_eq!(loaded.provenance, ServerProvenance::Custom);
        // The registry fixture must not drift into the custom bucket.
        insert_server_conn(&conn, &sample_http_server("srv-reg", "https://x.io/mcp")).unwrap();
        assert_eq!(
            get_server_conn(&conn, "srv-reg").unwrap().provenance,
            ServerProvenance::Registry
        );
    }

    /// A row written before the `provenance` column existed must re-hydrate as a
    /// registry install — `ADD COLUMN … DEFAULT 'registry'` backfills it, and
    /// mislabelling an existing catalog install as custom would expose it to
    /// hand-editing that the next catalog re-resolve would silently revert.
    #[test]
    fn pre_migration_rows_backfill_to_registry_provenance() {
        let (_f, conn) = open_test_conn();
        // Simulate the pre-migration shape by dropping the column back off.
        conn.execute_batch("ALTER TABLE mcp_servers DROP COLUMN provenance;")
            .expect("drop provenance column to emulate a pre-migration DB");
        assert!(!mcp_servers_columns(&conn)
            .unwrap()
            .iter()
            .any(|c| c == "provenance"));

        // Write the legacy row *while the column is absent* — that is the only
        // way the DDL default is what supplies the value. Inserting after the
        // migration would bind provenance explicitly and pass no matter what the
        // default says: flip it to 'custom' and every existing catalog install
        // would re-hydrate as Custom, which `refresh_existing_install` now
        // refuses — breaking token rotation for every installed server.
        conn.execute(
            "INSERT INTO mcp_servers
                (server_id, qualified_name, display_name, command_kind, command,
                 args_json, env_keys_json, installed_at)
             VALUES ('srv-legacy', 'ai.acme/legacy', 'Legacy', 'node', 'npx',
                     '[]', '[]', 0)",
            [],
        )
        .expect("insert a row with no provenance column, as a pre-migration build would");

        // Re-running init_schema is what happens on the next launch.
        init_schema(&conn).unwrap();
        assert!(mcp_servers_columns(&conn)
            .unwrap()
            .iter()
            .any(|c| c == "provenance"));

        assert_eq!(
            get_server_conn(&conn, "srv-legacy").unwrap().provenance,
            ServerProvenance::Registry
        );
    }

    /// The edit path replaces connection details while leaving identity and
    /// provenance alone — a rename must not re-key the row (which would orphan
    /// its env values) or relabel where it came from.
    #[test]
    fn update_server_custom_fields_preserves_identity() {
        let (_f, conn) = open_test_conn();
        let mut original = sample_server("srv-edit");
        original.qualified_name = "custom/original".to_string();
        original.provenance = ServerProvenance::Custom;
        insert_server_conn(&conn, &original).unwrap();

        let mut edited = original.clone();
        edited.display_name = "Renamed".to_string();
        edited.command = "uvx".to_string();
        edited.args = vec!["thing".to_string()];
        edited.transport = Transport::HttpRemote {
            url: "https://x.io/mcp".to_string(),
        };
        edited.command_kind = CommandKind::Python;
        // Fields the caller must not be able to move.
        edited.qualified_name = "custom/renamed".to_string();
        edited.installed_at = 999;

        update_server_custom_fields_conn(&conn, "srv-edit", &edited).unwrap();

        let loaded = get_server_conn(&conn, "srv-edit").unwrap();
        assert_eq!(loaded.display_name, "Renamed");
        assert_eq!(loaded.command, "uvx");
        assert_eq!(loaded.args, vec!["thing".to_string()]);
        assert_eq!(
            loaded.transport,
            Transport::HttpRemote {
                url: "https://x.io/mcp".to_string()
            }
        );
        assert_eq!(
            loaded.qualified_name, "custom/original",
            "identity is immutable"
        );
        assert_eq!(
            loaded.installed_at, 1_700_000_000_000,
            "install time is immutable"
        );
        assert_eq!(loaded.provenance, ServerProvenance::Custom);
    }

    #[test]
    fn insert_and_list_servers() {
        let (_f, conn) = open_test_conn();
        let server = sample_server("srv-1");
        insert_server_conn(&conn, &server).unwrap();
        let servers = list_servers_conn(&conn).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].server_id, "srv-1");
        assert_eq!(servers[0].command_kind, CommandKind::Node);
    }

    #[test]
    fn update_server_config_round_trips_and_clears() {
        let (_f, conn) = open_test_conn();
        insert_server_conn(&conn, &sample_server("srv-cfg")).unwrap();
        // sample_server starts with no config.
        assert_eq!(get_server_conn(&conn, "srv-cfg").unwrap().config, None);
        // Setting a config blob persists and reads back identically.
        let cfg = serde_json::json!({ "mode": "fast", "n": 3 });
        update_server_config_conn(&conn, "srv-cfg", Some(&cfg)).unwrap();
        assert_eq!(get_server_conn(&conn, "srv-cfg").unwrap().config, Some(cfg));
        // None clears it back to NULL.
        update_server_config_conn(&conn, "srv-cfg", None).unwrap();
        assert_eq!(get_server_conn(&conn, "srv-cfg").unwrap().config, None);
    }

    #[test]
    fn insert_server_if_absent_dedups_on_qualified_name() {
        let (_f, conn) = open_test_conn();
        // First install of a service inserts the row.
        let mut first = sample_server("srv-a");
        first.qualified_name = "@dup/server".to_string();
        assert!(insert_server_if_absent_conn(&conn, &first).unwrap());
        // A second install of the SAME qualified_name (different server_id) is a
        // no-op — the count stays at one and the original row survives.
        let mut second = sample_server("srv-b");
        second.qualified_name = "@dup/server".to_string();
        assert!(!insert_server_if_absent_conn(&conn, &second).unwrap());
        let rows: Vec<_> = list_servers_conn(&conn)
            .unwrap()
            .into_iter()
            .filter(|s| s.qualified_name == "@dup/server")
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].server_id, "srv-a");
    }

    #[test]
    fn find_server_by_qualified_name_returns_earliest_install() {
        let (_f, conn) = open_test_conn();
        // Two installs of the same service (different ids, different times).
        let mut early = sample_server("srv-early");
        early.installed_at = 100;
        let mut late = sample_server("srv-late");
        late.installed_at = 200;
        // Insert the later one first to prove ordering is by installed_at.
        insert_server_conn(&conn, &late).unwrap();
        insert_server_conn(&conn, &early).unwrap();

        let found = find_server_by_qualified_name_conn(&conn, "@test/server")
            .unwrap()
            .expect("server present");
        assert_eq!(found.server_id, "srv-early");

        assert!(find_server_by_qualified_name_conn(&conn, "@nope/missing")
            .unwrap()
            .is_none());
    }

    #[test]
    fn get_server_not_found() {
        let (_f, conn) = open_test_conn();
        let err = get_server_conn(&conn, "missing").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn delete_server_returns_true_when_found() {
        let (_f, conn) = open_test_conn();
        let server = sample_server("srv-del");
        insert_server_conn(&conn, &server).unwrap();

        // Open a config-less wrapper that reuses the same connection path
        let deleted = conn
            .execute(
                "DELETE FROM mcp_servers WHERE server_id = ?1",
                params!["srv-del"],
            )
            .unwrap();
        assert_eq!(deleted, 1);
    }

    #[test]
    fn env_values_upsert_and_load() {
        let (_f, conn) = open_test_conn();
        let server = sample_server("srv-env");
        insert_server_conn(&conn, &server).unwrap();

        let mut env = std::collections::HashMap::new();
        env.insert("API_KEY".to_string(), "secret123".to_string());
        set_env_values_conn(&conn, "srv-env", &env).unwrap();

        let loaded = load_env_values_conn(&conn, "srv-env").unwrap();
        assert_eq!(loaded.get("API_KEY").map(String::as_str), Some("secret123"));
    }

    #[test]
    fn registry_cache_miss_on_empty_db() {
        let (_f, conn) = open_test_conn();
        let cached = get_cached_conn(&conn, "search:rust").unwrap();
        assert!(cached.is_none());
    }

    #[test]
    fn registry_cache_hit_within_ttl() {
        let (_f, conn) = open_test_conn();
        set_cached_conn(&conn, "search:rust", r#"{"servers":[]}"#).unwrap();
        let cached = get_cached_conn(&conn, "search:rust").unwrap();
        assert!(cached.is_some());
    }

    #[test]
    fn registry_cache_miss_after_ttl() {
        let (_f, conn) = open_test_conn();
        // Insert with an old timestamp (way past TTL)
        let old_ts = now_ms() - REGISTRY_CACHE_TTL_MS - 1_000;
        conn.execute(
            "INSERT INTO mcp_registry_cache (cache_key, body_json, cached_at) VALUES (?1, ?2, ?3)",
            params!["stale:key", r#"{"servers":[]}"#, old_ts],
        )
        .unwrap();
        let cached = get_cached_conn(&conn, "stale:key").unwrap();
        assert!(cached.is_none());
    }

    #[test]
    fn server_args_and_env_keys_roundtrip_through_json() {
        let (_f, conn) = open_test_conn();
        let mut server = sample_server("srv-args");
        server.args = vec!["--port".to_string(), "8080".to_string()];
        server.env_keys = vec!["KEY_A".to_string(), "KEY_B".to_string()];
        insert_server_conn(&conn, &server).unwrap();

        let loaded = get_server_conn(&conn, "srv-args").unwrap();
        assert_eq!(loaded.args, vec!["--port", "8080"]);
        assert_eq!(loaded.env_keys, vec!["KEY_A", "KEY_B"]);
    }

    /// HTTP-remote row round-trips through INSERT/SELECT with the
    /// `deployment_url` preserved and `transport.dispatch_kind()` flipped
    /// to `"http_remote"`. Without this test a regression in the
    /// `map_server_row` column indices would silently downgrade every
    /// HTTP-remote install back to stdio at next launch.
    #[test]
    fn http_remote_server_roundtrips_with_url_preserved() {
        let (_f, conn) = open_test_conn();
        let server = sample_http_server("srv-http", "https://smithery.ai/server/x/mcp");
        insert_server_conn(&conn, &server).unwrap();

        let loaded = get_server_conn(&conn, "srv-http").unwrap();
        match loaded.transport {
            Transport::HttpRemote { url } => {
                assert_eq!(url, "https://smithery.ai/server/x/mcp");
            }
            other => panic!("expected HttpRemote, got {other:?}"),
        }
    }

    /// Mixed stdio + http rows list back in their persisted form (no
    /// cross-contamination of the `transport` column between rows).
    #[test]
    fn list_servers_preserves_per_row_transport() {
        let (_f, conn) = open_test_conn();
        insert_server_conn(&conn, &sample_server("srv-stdio")).unwrap();
        insert_server_conn(&conn, &sample_http_server("srv-http", "https://x.io/mcp")).unwrap();

        let mut servers = list_servers_conn(&conn).unwrap();
        servers.sort_by_key(|s| s.server_id.clone());
        assert_eq!(servers.len(), 2);
        // Alphabetical sort: "srv-http" precedes "srv-stdio".
        assert_eq!(servers[0].server_id, "srv-http");
        assert_eq!(
            servers[0].transport,
            Transport::HttpRemote {
                url: "https://x.io/mcp".to_string()
            }
        );
        assert_eq!(servers[1].server_id, "srv-stdio");
        assert_eq!(servers[1].transport, Transport::Stdio);
    }

    #[test]
    fn enabled_defaults_true_and_roundtrips_false() {
        let (_f, conn) = open_test_conn();
        let mut server = sample_server("srv-en");
        insert_server_conn(&conn, &server).unwrap();
        let loaded = get_server_conn(&conn, "srv-en").unwrap();
        assert!(loaded.enabled, "new installs default to enabled");

        server.server_id = "srv-dis".to_string();
        server.enabled = false;
        insert_server_conn(&conn, &server).unwrap();
        let loaded = get_server_conn(&conn, "srv-dis").unwrap();
        assert!(!loaded.enabled);
    }

    #[test]
    fn update_enabled_flips_persisted_value() {
        let (_f, conn) = open_test_conn();
        let server = sample_server("srv-u");
        insert_server_conn(&conn, &server).unwrap();
        update_enabled_conn(&conn, "srv-u", false).unwrap();
        let loaded = get_server_conn(&conn, "srv-u").unwrap();
        assert!(!loaded.enabled);
        update_enabled_conn(&conn, "srv-u", true).unwrap();
        let loaded = get_server_conn(&conn, "srv-u").unwrap();
        assert!(loaded.enabled);
    }

    #[test]
    fn additive_enabled_migration_defaults_legacy_rows_true() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let conn = rusqlite::Connection::open(tmp.path()).unwrap();

        // Pre-migration schema (no `enabled` column).
        conn.execute_batch(
            "CREATE TABLE mcp_servers (
                server_id           TEXT PRIMARY KEY,
                qualified_name      TEXT NOT NULL,
                display_name        TEXT NOT NULL,
                description         TEXT,
                icon_url            TEXT,
                command_kind        TEXT NOT NULL DEFAULT 'node',
                command             TEXT NOT NULL,
                args_json           TEXT NOT NULL DEFAULT '[]',
                env_keys_json       TEXT NOT NULL DEFAULT '[]',
                config_json         TEXT,
                installed_at        INTEGER NOT NULL,
                last_connected_at   INTEGER,
                transport           TEXT NOT NULL DEFAULT 'stdio',
                deployment_url      TEXT
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mcp_servers
                (server_id, qualified_name, display_name, command_kind, command, installed_at)
             VALUES ('legacy-en', '@old/server', 'Old', 'node', 'npx', 1700000000000)",
            [],
        )
        .unwrap();

        init_schema(&conn).unwrap();
        init_schema(&conn).unwrap(); // idempotent

        let loaded = get_server_conn(&conn, "legacy-en").unwrap();
        assert!(loaded.enabled, "legacy rows default enabled=true");
    }

    /// Simulates the pre-migration state by dropping the `transport` and
    /// `deployment_url` columns *after* schema init, manually inserting a
    /// row that lacks them, and then re-running `init_schema` to confirm
    /// the additive ALTER TABLE re-introduces the columns idempotently and
    /// the old row loads as stdio (the migration's whole point).
    ///
    /// SQLite can't `DROP COLUMN` portably before 3.35, so the test uses
    /// a CREATE-TABLE-AS rebuild to mimic the original schema shape.
    #[test]
    fn additive_migration_recovers_pre_migration_row_as_stdio() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let conn = rusqlite::Connection::open(tmp.path()).unwrap();

        // Step 1: pre-migration schema (no transport / deployment_url).
        conn.execute_batch(
            "CREATE TABLE mcp_servers (
                server_id           TEXT PRIMARY KEY,
                qualified_name      TEXT NOT NULL,
                display_name        TEXT NOT NULL,
                description         TEXT,
                icon_url            TEXT,
                command_kind        TEXT NOT NULL DEFAULT 'node',
                command             TEXT NOT NULL,
                args_json           TEXT NOT NULL DEFAULT '[]',
                env_keys_json       TEXT NOT NULL DEFAULT '[]',
                config_json         TEXT,
                installed_at        INTEGER NOT NULL,
                last_connected_at   INTEGER
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mcp_servers
                (server_id, qualified_name, display_name, command_kind, command, installed_at)
             VALUES ('legacy-1', '@old/server', 'Old', 'node', 'npx', 1700000000000)",
            [],
        )
        .unwrap();

        // Step 2: simulate the upgrade path — re-run init_schema, which
        // detects the missing columns via PRAGMA and runs ALTER TABLE.
        init_schema(&conn).unwrap();

        // Idempotency: running it again must not fail or duplicate the
        // columns. (Real launches hit this every process start.)
        init_schema(&conn).unwrap();

        // Step 3: the legacy row loads as Transport::Stdio.
        let loaded = get_server_conn(&conn, "legacy-1").unwrap();
        assert_eq!(loaded.transport, Transport::Stdio);
        assert_eq!(loaded.command, "npx");
    }

    /// #4194: the additive migration's PRAGMA-then-ALTER is not atomic across
    /// the several connections one MCP page load opens, so two can both see a
    /// column missing and both ALTER — the loser hitting "duplicate column
    /// name". `add_column_idempotent` must treat that exact error as success
    /// (the column exists either way) so it never surfaces as a UI error banner.
    #[test]
    fn add_column_idempotent_tolerates_duplicate_column() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let conn = rusqlite::Connection::open(tmp.path()).unwrap();
        conn.execute_batch("CREATE TABLE mcp_servers (server_id TEXT PRIMARY KEY);")
            .unwrap();

        const DDL: &str = "ALTER TABLE mcp_servers ADD COLUMN deployment_url TEXT";

        // First add succeeds.
        add_column_idempotent(&conn, DDL, "deployment_url column to mcp_servers").unwrap();
        assert!(mcp_servers_columns(&conn)
            .unwrap()
            .iter()
            .any(|c| c == "deployment_url"));

        // Re-running the SAME ALTER (the lost race) must NOT error — the column
        // already existing is the desired post-condition.
        add_column_idempotent(&conn, DDL, "deployment_url column to mcp_servers")
            .expect("duplicate column must be tolerated, not surfaced");
    }

    /// Guard against over-swallowing: a genuine DDL failure (here, a syntax
    /// error) must still propagate so real migration bugs are not hidden.
    #[test]
    fn add_column_idempotent_propagates_other_errors() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let conn = rusqlite::Connection::open(tmp.path()).unwrap();
        conn.execute_batch("CREATE TABLE mcp_servers (server_id TEXT PRIMARY KEY);")
            .unwrap();

        let err = add_column_idempotent(
            &conn,
            "ALTER TABLE mcp_servers ADD COLUMN", // malformed DDL
            "bogus column to mcp_servers",
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("Failed to add bogus column to mcp_servers"),
            "unexpected error: {err}"
