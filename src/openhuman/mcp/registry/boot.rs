//! Boot-time spawn of installed local MCP servers.

use futures::stream::StreamExt;

use crate::openhuman::config::Config;

use super::types::InstalledServer;
use super::{connections, store};

pub const BOOT_SPAWN_CONCURRENCY: usize = 8;

pub async fn spawn_installed_servers(config: &Config) {
    let servers = match store::list_servers(config) {
        Ok(servers) => servers,
        Err(error) => {
            tracing::warn!("[mcp-registry] boot: list_servers failed: {error}");
            return;
        }
    };
    if servers.is_empty() {
        tracing::debug!("[mcp-registry] boot: no installed servers to spawn");
        return;
    }
    tracing::info!("[mcp-registry] boot: spawning {} installed server(s)", servers.len());
    spawn_servers_concurrently(servers, |server| async move {
        connections::connect(config, &server).await.map(|tools| tools.len())
    }).await;
}

pub(crate) async fn spawn_servers_concurrently<F, Fut>(servers: Vec<InstalledServer>, connect_fn: F)
where
    F: Fn(InstalledServer) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<usize>>,
{
    futures::stream::iter(servers.into_iter().filter(|server| {
        if !server.enabled {
            tracing::info!("[mcp-registry] boot: skipping disabled server_id={} qualified={}", server.server_id, server.qualified_name);
        }
        server.enabled
    }))
    .for_each_concurrent(BOOT_SPAWN_CONCURRENCY, |server| {
        let server_id = server.server_id.clone();
        let qualified = server.qualified_name.clone();
        let connect_fn = &connect_fn;
        async move {
            match connect_fn(server).await {
                Ok(tool_count) => tracing::info!("[mcp-registry] boot: connected server_id={} qualified={} tools={}", server_id, qualified, tool_count),
                Err(error) => tracing::warn!("[mcp-registry] boot: connect failed server_id={} qualified={} err={error}", server_id, qualified),
            }
        }
    }).await;
}

#[cfg(test)]
#[path = "boot_tests.rs"]
mod tests;
