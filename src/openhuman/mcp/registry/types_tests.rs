    fn command_kind_roundtrip() {
        assert_eq!(CommandKind::parse("node").as_str(), "node");
        assert_eq!(CommandKind::parse("python").as_str(), "python");
        assert_eq!(CommandKind::parse("binary").as_str(), "binary");
        assert_eq!(CommandKind::parse("unknown").as_str(), "node");
    }

    #[test]
    fn server_status_as_str() {
        assert_eq!(ServerStatus::Connected.as_str(), "connected");
        assert_eq!(ServerStatus::Disconnected.as_str(), "disconnected");
        assert_eq!(ServerStatus::Connecting.as_str(), "connecting");
        assert_eq!(ServerStatus::Unauthorized.as_str(), "unauthorized");
        assert_eq!(ServerStatus::Error.as_str(), "error");
        assert_eq!(ServerStatus::Disabled.as_str(), "disabled");
    }

    #[test]
    fn smithery_server_summary_tolerates_missing_optional_fields() {
        let raw = json!({
            "qualifiedName": "@test/server",
            "displayName": "Test Server"
        });
        let s: SmitheryServerSummary = serde_json::from_value(raw).unwrap();
        assert_eq!(s.qualified_name, "@test/server");
        assert!(s.description.is_none());
        assert_eq!(s.use_count, 0);
        assert!(!s.is_deployed);
    }

    #[test]
    fn smithery_list_response_parses_pagination() {
        let raw = json!({
            "servers": [],
            "pagination": {
                "currentPage": 1,
                "pageSize": 20,
                "totalPages": 3,
                "totalCount": 55
            }
        });
        let resp: SmitheryListResponse = serde_json::from_value(raw).unwrap();
        assert_eq!(resp.pagination.current_page, 1);
        assert_eq!(resp.pagination.total_pages, 3);
        assert_eq!(resp.pagination.total_count, 55);
    }

    #[test]
    fn installed_server_serializes_without_env_values() {
        let server = InstalledServer {
            server_id: "uuid-1".to_string(),
            qualified_name: "@test/server".to_string(),
            display_name: "Test".to_string(),
            description: None,
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
        };
        let v = serde_json::to_value(&server).unwrap();
        // env_keys present, but no raw values
        assert!(v.get("env_keys").is_some());
        assert!(v.get("env_values").is_none());
    }

    /// `provenance` round-trips as the snake_case string the store column holds,
    /// and an absent field (every row written before the column existed)
    /// re-hydrates as a registry install rather than defaulting a catalog
    /// server into the hand-edited bucket.
    #[test]
    fn server_provenance_round_trips_and_defaults_to_registry() {
        assert_eq!(ServerProvenance::parse("custom"), ServerProvenance::Custom);
        assert_eq!(
            ServerProvenance::parse("registry"),
            ServerProvenance::Registry
        );
        assert_eq!(ServerProvenance::parse(""), ServerProvenance::Registry);
        assert_eq!(
            ServerProvenance::parse("garbage"),
            ServerProvenance::Registry
        );
        assert_eq!(ServerProvenance::Custom.as_str(), "custom");
        assert_eq!(ServerProvenance::Registry.as_str(), "registry");

        let json = serde_json::json!({
            "server_id": "uuid-legacy",
            "qualified_name": "@test/legacy",
            "display_name": "Legacy",
            "command_kind": "node",
            "command": "npx",
            "args": [],
            "env_keys": [],
            "installed_at": 1_700_000_000_000_i64,
        });
        let server: InstalledServer =
            serde_json::from_value(json).expect("legacy payload without `provenance` deserialises");
        assert_eq!(server.provenance, ServerProvenance::Registry);
    }

    /// `Transport::dispatch_kind` is the column value persisted into
    /// `mcp_servers.transport`. Pinning both stdio and http-remote so a
    /// schema-side change can't silently rename one without surfacing here.
    #[test]
    fn transport_dispatch_kind_strings_are_stable() {
        assert_eq!(Transport::Stdio.dispatch_kind(), "stdio");
        assert_eq!(
            Transport::HttpRemote {
                url: "https://example.com/mcp".to_string()
            }
            .dispatch_kind(),
            "http_remote"
        );
    }

    /// `Transport::parse` is what the store layer calls when re-hydrating
    /// a row. The Stdio fallback for unknown / missing values is the
    /// migration-safety hatch — rows persisted before the `transport`
    /// column existed must keep working as stdio installs.
    #[test]
    fn transport_parse_falls_back_to_stdio_for_unknown_kinds() {
        // Stdio: explicit + with-no-url
        assert_eq!(Transport::parse("stdio", None), Transport::Stdio);
        assert_eq!(Transport::parse("stdio", Some("ignored")), Transport::Stdio);

        // Pre-migration empty value → stdio (backwards-compat).
        assert_eq!(Transport::parse("", None), Transport::Stdio);
        // Unknown kind from a future row → stdio (defensive default; we'd
        // rather a misconfigured row stall on connect than misroute).
        assert_eq!(Transport::parse("garbage", None), Transport::Stdio);

        // HTTP remote round-trip carries the URL through.
        assert_eq!(
            Transport::parse("http_remote", Some("https://x.io/mcp")),
            Transport::HttpRemote {
                url: "https://x.io/mcp".to_string()
            }
        );
    }

    /// `deployment_url` accessor is what the store uses to populate the
    /// adjacent `mcp_servers.deployment_url` column. Stdio → `None`,
    /// HTTP remote → `Some(url)`. Confirms the two never get crossed.
    #[test]
    fn transport_deployment_url_accessor() {
        assert_eq!(Transport::Stdio.deployment_url(), None);
        let http = Transport::HttpRemote {
            url: "https://smithery.ai/server/x".to_string(),
        };
        assert_eq!(http.deployment_url(), Some("https://smithery.ai/server/x"));
    }

    /// `InstalledServer::transport` is `#[serde(default)]`-backed so that
    /// pre-migration JSON payloads (without the field at all) deserialise
    /// as stdio installs. Without this, every persisted row from before
    /// this change would fail to load after upgrade.
    #[test]
    fn installed_server_defaults_transport_to_stdio_on_missing_field() {
        let legacy = json!({
            "server_id": "uuid-1",
            "qualified_name": "@old/server",
            "display_name": "Old",
            "description": null,
            "icon_url": null,
            "command_kind": "node",
            "command": "npx",
            "args": ["-y", "@old/server"],
            "env_keys": [],
            "config": null,
            "installed_at": 1_700_000_000_000i64,
            "last_connected_at": null
            // ← deliberately no `transport` or `enabled` key
        });
        let s: InstalledServer = serde_json::from_value(legacy).unwrap();
        assert_eq!(s.transport, Transport::Stdio);
        assert!(
            s.enabled,
            "enabled should default to true when field is absent"
        );
    }

    /// Smithery API sends camelCase; the official adapter builds snake_case
    /// in `into_summary()`. Both must deserialize into the same struct.
    #[test]
    fn smithery_summary_deserializes_from_snake_case() {
        let raw = json!({
            "qualified_name": "@test/snake",
            "display_name": "Snake Test",
            "icon_url": "https://example.com/icon.png",
            "use_count": 42,
            "is_deployed": true,
        });
        let s: SmitheryServerSummary = serde_json::from_value(raw).unwrap();
        assert_eq!(s.qualified_name, "@test/snake");
        assert_eq!(s.display_name, "Snake Test");
        assert_eq!(s.icon_url.as_deref(), Some("https://example.com/icon.png"));
        assert_eq!(s.use_count, 42);
        assert!(s.is_deployed);
    }

    /// `website_url`/`auth_kind` are adapter-derived trust signals that drive the
    /// strict "perfect server" filter and the UI. They must NEVER be honored from
    /// the wire — `skip_deserializing` forces them to `None` on any parse so a
    /// payload that starts emitting the keys can't spoof curation. Pins the
    /// annotation so a future serde change can't silently re-admit them.
    #[test]
    fn smithery_summary_never_deserializes_trust_signals_from_the_wire() {
        let raw = json!({
            "qualifiedName": "@evil/server",
            "displayName": "Evil",
            "website_url": "https://spoofed.example",
            "auth_kind": "api_key",
        });
        let s: SmitheryServerSummary = serde_json::from_value(raw).unwrap();
        assert_eq!(
            s.website_url, None,
            "website_url must not come from the wire"
        );
        assert_eq!(s.auth_kind, None, "auth_kind must not come from the wire");
    }

    /// RPC responses to the frontend must use snake_case field names.
    /// This pins the serialization format so a future serde annotation
    /// change doesn't silently break the frontend.
    #[test]
    fn smithery_summary_serializes_as_snake_case() {
        let s = SmitheryServerSummary {
            qualified_name: "@test/ser".to_string(),
            display_name: "Ser Test".to_string(),
            description: Some("desc".to_string()),
            icon_url: Some("https://example.com/i.png".to_string()),
            use_count: 10,
            is_deployed: true,
            source: "mcp_official".to_string(),
            official: false,
            website_url: None,
            auth_kind: None,
            extra: Default::default(),
        };
        let v = serde_json::to_value(&s).unwrap();
        assert!(
            v.get("qualified_name").is_some(),
            "expected snake_case qualified_name"
        );
        assert!(
            v.get("display_name").is_some(),
            "expected snake_case display_name"
        );
        assert!(v.get("icon_url").is_some(), "expected snake_case icon_url");
        assert!(
            v.get("use_count").is_some(),
            "expected snake_case use_count"
        );
        assert!(
            v.get("is_deployed").is_some(),
            "expected snake_case is_deployed"
        );
        // Must NOT have camelCase keys
        assert!(
            v.get("qualifiedName").is_none(),
            "must not serialize as camelCase"
        );
        assert!(
            v.get("displayName").is_none(),
            "must not serialize as camelCase"
        );
    }

    /// Same snake_case serialization pin for SmitheryServerDetail.
    #[test]
    fn smithery_detail_serializes_as_snake_case() {
        let d = SmitheryServerDetail {
            qualified_name: "@test/d".to_string(),
            display_name: "Detail".to_string(),
            description: None,
            icon_url: None,
            connections: vec![],
            source: "smithery".to_string(),
            extra: Default::default(),
        };
        let v = serde_json::to_value(&d).unwrap();
        assert!(
            v.get("qualified_name").is_some(),
            "expected snake_case qualified_name"
        );
        assert!(
            v.get("display_name").is_some(),
            "expected snake_case display_name"
        );
        assert!(
            v.get("qualifiedName").is_none(),
            "must not serialize as camelCase"
        );
    }

    /// SmitheryConnection must serialize with snake_case for the frontend.
    #[test]
    fn smithery_connection_serializes_as_snake_case() {
        let c = SmitheryConnection {
            r#type: "stdio".to_string(),
            deployment_url: Some("https://x.com".to_string()),
            config_schema: None,
            example_config: Some(json!({"command": "npx"})),
            published: true,
            extra: Default::default(),
        };
        let v = serde_json::to_value(&c).unwrap();
        assert!(
            v.get("deployment_url").is_some(),
            "expected snake_case deployment_url"
        );
        assert!(
            v.get("config_schema").is_some(),
            "expected snake_case config_schema"
        );
        assert!(
            v.get("example_config").is_some(),
            "expected snake_case example_config"
        );
        assert!(
            v.get("deploymentUrl").is_none(),
            "must not serialize as camelCase"
        );
    }

    /// SmitheryConnection must also deserialize from Smithery's camelCase wire format.
    #[test]
    fn smithery_connection_deserializes_from_camel_case() {
        let raw = json!({
            "type": "stdio",
            "deploymentUrl": "https://x.com",
            "configSchema": { "properties": {} },
            "exampleConfig": { "command": "npx" },
            "published": true,
        });
        let c: SmitheryConnection = serde_json::from_value(raw).unwrap();
        assert_eq!(c.deployment_url.as_deref(), Some("https://x.com"));
        assert!(c.config_schema.is_some());
        assert!(c.example_config.is_some());
    }

    #[test]
    fn conn_status_status_field_serializes_lowercase() {
        let s = ConnStatus {
            server_id: "s1".to_string(),
            qualified_name: "@test/s".to_string(),
            display_name: "S".to_string(),
            status: ServerStatus::Connected,
            tool_count: 3,
            last_error: None,
            auth_hint: None,
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["status"], json!("connected"));
        // `auth_hint` is omitted from the wire when absent (skip_serializing_if).
use super::*;
use serde_json::json;
