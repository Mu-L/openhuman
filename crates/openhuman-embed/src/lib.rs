//! Stable library facade for products that embed OpenHuman in-process.
//!
//! This package gives hosts such as Medulla and OpenCompany an intentionally
//! small dependency surface while [`openhuman_core`] remains the implementation
//! crate. Build a [`CoreRuntime`], wrap it in [`Core`], and call typed domain
//! facades; or use [`Harness`] for the higher-level prompt-to-reply API.
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use std::sync::Arc;
//! use openhuman_embed::{Core, CoreBuilder, DomainSet, HostKind, ServiceSet};
//!
//! let runtime = CoreBuilder::new(HostKind::Library)
//!     .domains(DomainSet::embedded())
//!     .services(ServiceSet::none())
//!     .build()
//!     .await?;
//! let core = Core::from_runtime(Arc::new(runtime));
//! let flags = core.config().runtime_flags().await?;
//! println!("log_prompts={}", flags.log_prompts);
//! # Ok(())
//! # }
//! ```
//!
//! Cargo features forward to the implementation crate. Disable defaults for a
//! narrow host and opt into only the domains it needs.

pub use openhuman_core::agent_progress;
pub use openhuman_core::embed::*;
pub use openhuman_core::openhuman::tools::toolpacks::{GroupMode, ToolGroups};
pub use openhuman_core::{
    CoreBuilder, CoreRuntime, DaemonConfig, DomainSet, HostKind, ServiceSet, TokenSource,
};

pub use openhuman_core::api::{product_identity, set_product_identity, ProductIdentity};
