//! SQLite persistence for the MCP clients domain.
//!
//! Uses `mcp_clients/mcp_clients.db` inside the workspace directory.
//! Three tables:
//!   - `mcp_servers`     — installed server metadata (no env values)
//!   - `mcp_client_env`  — per-server env values (key + value; values never
//!                          leave this module or appear in responses)
//!   - `mcp_registry_cache` — Smithery API response cache with TTL

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension as _};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::openhuman::config::Config;

use super::types::{CommandKind, InstalledServer, ServerProvenance, Transport};

// ── Helpers ──────────────────────────────────────────────────────────────────

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn with_connection<T>(config: &Config, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let db_dir = config.workspace_dir.join("mcp_clients");
    std::fs::create_dir_all(&db_dir)
        .with_context(|| format!("Failed to create mcp_clients dir: {}", db_dir.display()))?;
    let db_path = db_dir.join("mcp_clients.db");
    let conn = Connection::open(&db_path)
        .with_context(|| format!("Failed to open mcp_clients DB: {}", db_path.display()))?;
    // SQLite's default busy handler is null: a writer that finds the DB locked
    // fails immediately with SQLITE_BUSY. Several writers share this file — the
    // 60s supervisor tick's `update_last_connected`, OAuth refresh's
    // `persist_tokens`, and the custom-server RPCs — so the default turns routine
    // contention into a surfaced error. Wait instead.
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .context("Failed to set busy_timeout on mcp_clients DB")?;
    init_schema(&conn)?;
    f(&conn)
}

/// Build the schema using an in-memory path (for tests).
pub fn with_test_connection<T>(
    db_path: &Path,
    f: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("open test DB: {}", db_path.display()))?;
    init_schema(&conn)?;
    f(&conn)
}

fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;

         CREATE TABLE IF NOT EXISTS mcp_servers (
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
         );

         CREATE TABLE IF NOT EXISTS mcp_client_env (
             server_id   TEXT NOT NULL,
             key         TEXT NOT NULL,
             value       TEXT NOT NULL,
             PRIMARY KEY (server_id, key),
             FOREIGN KEY (server_id) REFERENCES mcp_servers(server_id) ON DELETE CASCADE
         );

         CREATE TABLE IF NOT EXISTS mcp_registry_cache (
             cache_key   TEXT PRIMARY KEY,
             body_json   TEXT NOT NULL,
             cached_at   INTEGER NOT NULL
         );",
    )
    .context("Failed to initialise mcp_clients schema")?;

    // Additive HTTP-remote transport columns (introduced after the schema
    // was first cut). SQLite's `ALTER TABLE ADD COLUMN` doesn't support
    // `IF NOT EXISTS`, so we use `PRAGMA table_info` to detect which
    // columns are already there and skip the ones that are. Idempotent
    // across launches; old `'stdio'`-implicit rows pick up the new
    // `transport` column with the default value.
    let existing_cols = mcp_servers_columns(conn)?;
    if !existing_cols.iter().any(|c| c == "transport") {
        add_column_idempotent(
            conn,
            "ALTER TABLE mcp_servers ADD COLUMN transport TEXT NOT NULL DEFAULT 'stdio'",
            "transport column to mcp_servers",
        )?;
    }
    if !existing_cols.iter().any(|c| c == "deployment_url") {
        add_column_idempotent(
            conn,
            "ALTER TABLE mcp_servers ADD COLUMN deployment_url TEXT",
            "deployment_url column to mcp_servers",
        )?;
    }
    if !existing_cols.iter().any(|c| c == "enabled") {
        add_column_idempotent(
            conn,
            "ALTER TABLE mcp_servers ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1",
            "enabled column to mcp_servers",
        )?;
    }
    // Distinguishes catalog installs from hand-entered ones. Every row that
    // predates this column arrived through `mcp_clients_install`, i.e. from a
    // registry, so the default backfills existing installs correctly.
    if !existing_cols.iter().any(|c| c == "provenance") {
        add_column_idempotent(
            conn,
            "ALTER TABLE mcp_servers ADD COLUMN provenance TEXT NOT NULL DEFAULT 'registry'",
            "provenance column to mcp_servers",
        )?;
    }

    Ok(())
}

/// Run an additive `ALTER TABLE … ADD COLUMN`, treating an "already exists"
/// failure as success.
///
/// The `PRAGMA table_info` snapshot in [`init_schema`] skips the ALTER in the
/// common case, but that check-then-alter is not atomic *across connections*:
/// every store call opens its own [`Connection`] and runs `init_schema`, so the
/// several MCP RPCs a single page load fans out (list / status / registry) can
/// each snapshot the column as missing before any of them adds it — then all
/// race to `ALTER`, and every loser fails with "duplicate column name". SQLite's
/// `ADD COLUMN` has no `IF NOT EXISTS`, so we swallow exactly that error: the
/// column existing is the desired post-condition, and surfacing it turned a
/// benign race into the red "Failed to add deployment_url column to mcp_servers"
/// banner on the MCP Servers page (#4194). Any other failure still propagates.
fn add_column_idempotent(conn: &Connection, ddl: &str, what: &str) -> Result<()> {
    match conn.execute(ddl, []) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(_, Some(msg)))
            if msg.contains("duplicate column name") =>
        {
            log::debug!("[mcp_registry] {what} already present (concurrent migration) — skipping");
            Ok(())
        }
        Err(e) => Err(anyhow::Error::new(e).context(format!("Failed to add {what}"))),
    }
}

/// Snapshot of the column names on `mcp_servers`. Used by the additive
/// migration in [`init_schema`] to decide which `ALTER TABLE ADD COLUMN`
/// statements still need to run on this DB.
fn mcp_servers_columns(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(mcp_servers)")
        .context("prepare PRAGMA table_info")?;
    // PRAGMA table_info row shape: (cid, name, type, notnull, dflt_value, pk).
    let mut rows = stmt.query([])?;
    let mut cols = Vec::new();
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        cols.push(name);
    }
    Ok(cols)
}

// ── InstalledServer CRUD ──────────────────────────────────────────────────────

pub fn insert_server(config: &Config, server: &InstalledServer) -> Result<()> {
    with_connection(config, |conn| insert_server_conn(conn, server))
}

pub fn insert_server_conn(conn: &Connection, server: &InstalledServer) -> Result<()> {
    let args_json = serde_json::to_string(&server.args)?;
    let env_keys_json = serde_json::to_string(&server.env_keys)?;
    let config_json = server
        .config
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    conn.execute(
        "INSERT INTO mcp_servers
             (server_id, qualified_name, display_name, description, icon_url,
              command_kind, command, args_json, env_keys_json, config_json,
              installed_at, last_connected_at, transport, deployment_url, enabled,
              provenance)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            server.server_id,
            server.qualified_name,
            server.display_name,
            server.description,
            server.icon_url,
            server.command_kind.as_str(),
            server.command,
            args_json,
            env_keys_json,
            config_json,
            server.installed_at,
            server.last_connected_at,
            server.transport.dispatch_kind(),
            server.transport.deployment_url(),
            server.enabled as i64,
            server.provenance.as_str(),
        ],
    )
    .context("Failed to insert mcp_server")?;
    Ok(())
}

/// Insert a server row only if no row with the same `qualified_name` already
/// exists, in a single atomic statement. The `mcp_clients_install` flow checks
/// `find_server_by_qualified_name` before inserting, but an awaited
/// `registry_get` sits between that read and the write, so two concurrent
/// installs of the same service could both miss and insert (the PK is
/// `server_id`, which doesn't prevent duplicate `qualified_name`s). `INSERT …
/// SELECT … WHERE NOT EXISTS` closes that window without a schema change.
/// Returns `true` if this call inserted the row, `false` if one already existed.
pub fn insert_server_if_absent(config: &Config, server: &InstalledServer) -> Result<bool> {
    with_connection(config, |conn| insert_server_if_absent_conn(conn, server))
}

pub fn insert_server_if_absent_conn(conn: &Connection, server: &InstalledServer) -> Result<bool> {
    let args_json = serde_json::to_string(&server.args)?;
    let env_keys_json = serde_json::to_string(&server.env_keys)?;
    let config_json = server
        .config
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let n = conn
        .execute(
            "INSERT INTO mcp_servers
                     (server_id, qualified_name, display_name, description, icon_url,
                      command_kind, command, args_json, env_keys_json, config_json,
                      installed_at, last_connected_at, transport, deployment_url, enabled,
                      provenance)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16
                 WHERE NOT EXISTS (SELECT 1 FROM mcp_servers WHERE qualified_name = ?2)",
            params![
                server.server_id,
                server.qualified_name,
                server.display_name,
                server.description,
                server.icon_url,
                server.command_kind.as_str(),
                server.command,
                args_json,
                env_keys_json,
                config_json,
                server.installed_at,
                server.last_connected_at,
                server.transport.dispatch_kind(),
                server.transport.deployment_url(),
                server.enabled as i64,
                server.provenance.as_str(),
            ],
        )
        .context("Failed to insert mcp_server (if absent)")?;
    Ok(n > 0)
}

/// Update only the `env_keys` list for an installed server. Used by
/// `mcp_clients_update_env` to keep the persisted key-name list in sync with a
/// reconfigure — the env *values* live in the separate `mcp_client_env` table,
/// while the key-name list shown in list/status responses lives on the server
/// row. A plain `insert_server` would conflict on the primary key.
pub fn update_server_env_keys(config: &Config, server_id: &str, env_keys: &[String]) -> Result<()> {
    let env_keys_json = serde_json::to_string(env_keys)?;
    with_connection(config, |conn| {
        conn.execute(
            "UPDATE mcp_servers SET env_keys_json = ?2 WHERE server_id = ?1",
            params![server_id, env_keys_json],
        )
        .context("Failed to update mcp_server env_keys")?;
        Ok(())
    })
}

/// Update only the `config_json` blob for an installed server. Used by the
/// idempotent re-install path so a second install carrying new config refreshes
/// the existing row instead of dropping it — a plain `insert_server` would
/// conflict on the primary key. `None` clears the stored config.
pub fn update_server_config(
    config: &Config,
    server_id: &str,
    value: Option<&serde_json::Value>,
) -> Result<()> {
    with_connection(config, |conn| {
        update_server_config_conn(conn, server_id, value)
    })
}

pub fn update_server_config_conn(
    conn: &Connection,
    server_id: &str,
    value: Option<&serde_json::Value>,
) -> Result<()> {
    let config_json = value.map(serde_json::to_string).transpose()?;
    conn.execute(
        "UPDATE mcp_servers SET config_json = ?2 WHERE server_id = ?1",
        params![server_id, config_json],
    )
    .context("Failed to update mcp_server config")?;
    Ok(())
}

pub fn list_servers(config: &Config) -> Result<Vec<InstalledServer>> {
    with_connection(config, list_servers_conn)
}

pub fn list_servers_conn(conn: &Connection) -> Result<Vec<InstalledServer>> {
    let mut stmt = conn.prepare(
        "SELECT server_id, qualified_name, display_name, description, icon_url,
                command_kind, command, args_json, env_keys_json, config_json,
                installed_at, last_connected_at, transport, deployment_url, enabled,
                provenance
         FROM mcp_servers ORDER BY installed_at ASC",
    )?;
    let rows = stmt.query_map([], map_server_row)?;
    let mut servers = Vec::new();
    for row in rows {
        servers.push(row?);
    }
    Ok(servers)
}

/// First installed server with this qualified name, if any. The schema allows
/// multiple installs of the same `qualified_name` (the PK is `server_id`), so
/// this returns the earliest by `installed_at` — used to keep install
/// idempotent (one install per service).
pub fn find_server_by_qualified_name(
    config: &Config,
    qualified_name: &str,
) -> Result<Option<InstalledServer>> {
    with_connection(config, |conn| {
        find_server_by_qualified_name_conn(conn, qualified_name)
    })
}

pub fn find_server_by_qualified_name_conn(
    conn: &Connection,
    qualified_name: &str,
) -> Result<Option<InstalledServer>> {
    let mut stmt = conn.prepare(
        "SELECT server_id, qualified_name, display_name, description, icon_url,
                command_kind, command, args_json, env_keys_json, config_json,
                installed_at, last_connected_at, transport, deployment_url, enabled,
                provenance
         FROM mcp_servers WHERE qualified_name = ?1
         ORDER BY installed_at ASC LIMIT 1",
    )?;
    let mut rows = stmt.query(params![qualified_name])?;
    match rows.next()? {
        Some(row) => Ok(Some(map_server_row(row)?)),
        None => Ok(None),
    }
}

pub fn get_server(config: &Config, server_id: &str) -> Result<InstalledServer> {
    with_connection(config, |conn| get_server_conn(conn, server_id))
}

pub fn get_server_conn(conn: &Connection, server_id: &str) -> Result<InstalledServer> {
    let mut stmt = conn.prepare(
        "SELECT server_id, qualified_name, display_name, description, icon_url,
                command_kind, command, args_json, env_keys_json, config_json,
                installed_at, last_connected_at, transport, deployment_url, enabled,
                provenance
         FROM mcp_servers WHERE server_id = ?1",
    )?;
    let mut rows = stmt.query(params![server_id])?;
    if let Some(row) = rows.next()? {
        map_server_row(row).map_err(Into::into)
    } else {
        anyhow::bail!("MCP server '{}' not found", server_id)
    }
}

pub fn delete_server(config: &Config, server_id: &str) -> Result<bool> {
    with_connection(config, |conn| {
        let changed = conn
            .execute(
                "DELETE FROM mcp_servers WHERE server_id = ?1",
                params![server_id],
            )
            .context("Failed to delete mcp_server")?;
        Ok(changed > 0)
    })
}

pub fn update_last_connected(config: &Config, server_id: &str) -> Result<()> {
    let ts = now_ms();
    with_connection(config, |conn| {
        conn.execute(
            "UPDATE mcp_servers SET last_connected_at = ?1 WHERE server_id = ?2",
            params![ts, server_id],
        )
        .context("Failed to update last_connected_at")?;
        Ok(())
    })
}

fn map_server_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InstalledServer> {
    let args_json: String = row.get(7)?;
    let env_keys_json: String = row.get(8)?;
    let config_json: Option<String> = row.get(9)?;

    let args: Vec<String> = serde_json::from_str(&args_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let env_keys: Vec<String> = serde_json::from_str(&env_keys_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let config: Option<Value> = match config_json.as_deref() {
        None => None,
        Some(s) => Some(serde_json::from_str(s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(e))
        })?),
    };

    // Both transport columns are post-migration additions, so `row.get`
    // may return missing-column-by-name errors on a DB that hasn't run the
    // ADD COLUMN steps for some reason (rare — the migration is in
    // `init_schema`). Fall back to stdio rather than fail loading the
    // whole row.
    let transport_kind: String = row.get::<_, Option<String>>(12)?.unwrap_or_default();
    let deployment_url: Option<String> = row.get(13)?;
    let transport = Transport::parse(&transport_kind, deployment_url.as_deref());

    // `enabled` is a post-migration addition; fall back to `1` (true) for
    // any row that predates the column so legacy installs keep auto-connecting.
    let enabled: i64 = row.get::<_, Option<i64>>(14)?.unwrap_or(1);

    // `provenance` is likewise post-migration; an absent value means the row was
    // written before custom servers existed, so it can only be a registry
    // install.
    let provenance_raw: String = row.get::<_, Option<String>>(15)?.unwrap_or_default();
    let provenance = ServerProvenance::parse(&provenance_raw);

    Ok(InstalledServer {
        server_id: row.get(0)?,
        qualified_name: row.get(1)?,
        display_name: row.get(2)?,
        description: row.get(3)?,
        icon_url: row.get(4)?,
        command_kind: CommandKind::parse(&row.get::<_, String>(5)?),
        command: row.get(6)?,
        args,
        env_keys,
        config,
        installed_at: row.get(10)?,
        last_connected_at: row.get(11)?,
        transport,
        enabled: enabled != 0,
        provenance,
    })
}

/// Apply a custom-server edit whose env depends on what is currently stored, as
/// one serializable read-modify-write.
///
/// Writes only the fields the custom-server form owns — `server_id`,
/// `qualified_name`, `installed_at` and `provenance` stay put, so a rename can't
/// orphan the row's env or relabel a registry install as custom.
///
/// The caller's `build` closure receives the current record **and** the stored
/// env, both read *inside* the transaction, and returns the row to write plus
/// the resolved env (or an error, e.g. a provenance rejection). Everything the
/// edit decides on — provenance, the previous transport for credential-scope,
/// the env — is read under the lock. Reading any of it outside races a
/// concurrent edit: an OAuth refresh could rotate a token, or a concurrent
/// `update_custom` could change the transport and store new-scope credentials,
/// between a stale read and this write. Using the stale snapshot could revert
/// the token or mis-classify the scope and carry the new credentials across it.
/// `BEGIN IMMEDIATE` takes the write lock up front, so the read can't be
/// undercut and two concurrent edits serialize at BEGIN rather than deadlocking
/// on a `SHARED`→`RESERVED` upgrade. `get_server_conn` errors if the row was
/// removed in the gap, so a concurrent uninstall can't produce a phantom write.
pub fn update_custom_server_rmw<F>(
    config: &Config,
    server_id: &str,
    build: F,
) -> Result<InstalledServer>
where
    F: FnOnce(
        &InstalledServer,
        HashMap<String, String>,
    ) -> Result<(InstalledServer, HashMap<String, String>)>,
{
    with_connection(config, |conn| {
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let outcome = (|| {
            let current = get_server_conn(conn, server_id)?;
            let stored = load_env_values_conn(conn, server_id)?;
            let (server, env) = build(&current, stored)?;
            update_server_custom_fields_conn(conn, server_id, &server)?;
            set_env_values_conn(conn, server_id, &env)?;
            Ok::<InstalledServer, anyhow::Error>(server)
        })();
        match outcome {
            Ok(server) => {
                conn.execute_batch("COMMIT")?;
                Ok(server)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    })
}

/// Insert a new custom-server row and its env values as one transaction.
///
/// Returns `false` when the `qualified_name` was taken between allocation and
/// insert. Splitting the two writes would let the row commit and the env fail,
/// leaving a server the caller was told did not save: it holds the name, so a
/// retry allocates `-2`, and it is `enabled`, so the supervisor keeps launching
/// the user's command every 60s with no credentials.
pub fn insert_custom_server_with_env(
    config: &Config,
    server: &InstalledServer,
    env: &HashMap<String, String>,
) -> Result<bool> {
    with_connection(config, |conn| {
        let tx = conn.unchecked_transaction()?;
        if !insert_server_if_absent_conn(&tx, server)? {
            return Ok(false);
        }
        set_env_values_conn(&tx, &server.server_id, env)?;
        tx.commit()?;
        Ok(true)
    })
}

pub fn update_server_custom_fields_conn(
    conn: &Connection,
    server_id: &str,
    server: &InstalledServer,
) -> Result<()> {
    let args_json = serde_json::to_string(&server.args)?;
    let env_keys_json = serde_json::to_string(&server.env_keys)?;
    conn.execute(
        "UPDATE mcp_servers
            SET display_name = ?1, description = ?2, command_kind = ?3,
                command = ?4, args_json = ?5, env_keys_json = ?6,
                transport = ?7, deployment_url = ?8
          WHERE server_id = ?9",
        params![
            server.display_name,
            server.description,
            server.command_kind.as_str(),
            server.command,
            args_json,
            env_keys_json,
            server.transport.dispatch_kind(),
            server.transport.deployment_url(),
            server_id,
        ],
    )
    .context("Failed to update custom mcp_server fields")?;
    Ok(())
}

pub fn update_enabled(config: &Config, server_id: &str, enabled: bool) -> Result<()> {
    with_connection(config, |conn| update_enabled_conn(conn, server_id, enabled))
}

pub fn update_enabled_conn(conn: &Connection, server_id: &str, enabled: bool) -> Result<()> {
    conn.execute(
        "UPDATE mcp_servers SET enabled = ?2 WHERE server_id = ?1",
        params![server_id, enabled as i64],
    )
    .context("Failed to update mcp_server enabled flag")?;
    Ok(())
}

// ── Env values ───────────────────────────────────────────────────────────────

/// Store (insert or replace) env key-value pairs for a server.
/// Values are never returned in any list/status response.
pub fn set_env_values(
    config: &Config,
    server_id: &str,
    env: &std::collections::HashMap<String, String>,
) -> Result<()> {
    with_connection(config, |conn| set_env_values_conn(conn, server_id, env))
}

pub fn set_env_values_conn(
    conn: &Connection,
    server_id: &str,
    env: &std::collections::HashMap<String, String>,
) -> Result<()> {
    // Delete all existing env rows for this server first so that keys removed
    // from the new map don't linger.  The upsert below re-inserts the current set.
    conn.execute(
        "DELETE FROM mcp_client_env WHERE server_id = ?1",
        params![server_id],
    )
    .context("Failed to clear previous mcp_client_env rows")?;

    for (key, value) in env {
        conn.execute(
            "INSERT INTO mcp_client_env (server_id, key, value) VALUES (?1, ?2, ?3)",
            params![server_id, key, value],
        )
        .context("Failed to insert mcp_client_env")?;
    }
    Ok(())
}

/// Load env values for a server (used when spawning the subprocess).
/// NEVER serialize or log these values.
pub fn load_env_values(
    config: &Config,
    server_id: &str,
) -> Result<std::collections::HashMap<String, String>> {
    with_connection(config, |conn| load_env_values_conn(conn, server_id))
}

pub fn load_env_values_conn(
    conn: &Connection,
    server_id: &str,
) -> Result<std::collections::HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT key, value FROM mcp_client_env WHERE server_id = ?1")?;
    let rows = stmt.query_map(params![server_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut map = std::collections::HashMap::new();
    for row in rows {
        let (k, v) = row?;
        map.insert(k, v);
    }
    Ok(map)
}

// ── Registry cache ────────────────────────────────────────────────────────────

const REGISTRY_CACHE_TTL_MS: i64 = 10 * 60 * 1_000; // 10 minutes

pub fn get_cached(config: &Config, cache_key: &str) -> Result<Option<String>> {
    with_connection(config, |conn| get_cached_conn(conn, cache_key))
}

pub fn get_cached_conn(conn: &Connection, cache_key: &str) -> Result<Option<String>> {
    let now = now_ms();
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT body_json, cached_at FROM mcp_registry_cache WHERE cache_key = ?1",
            params![cache_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .context("Failed to query registry cache")?;

    match row {
        Some((body, cached_at)) if now - cached_at < REGISTRY_CACHE_TTL_MS => Ok(Some(body)),
        _ => Ok(None),
    }
}

pub fn set_cached(config: &Config, cache_key: &str, body_json: &str) -> Result<()> {
    with_connection(config, |conn| set_cached_conn(conn, cache_key, body_json))
}

pub fn set_cached_conn(conn: &Connection, cache_key: &str, body_json: &str) -> Result<()> {
    let now = now_ms();
    conn.execute(
        "INSERT OR REPLACE INTO mcp_registry_cache (cache_key, body_json, cached_at)
         VALUES (?1, ?2, ?3)",
        params![cache_key, body_json, now],
    )
    .context("Failed to upsert registry cache")?;
    Ok(())
}

#[cfg(test)]
mod store_tests;
