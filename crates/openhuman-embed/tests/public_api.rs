use std::sync::Arc;

use openhuman_embed::{
    set_product_identity, Access, AgentTurnOrigin, Core, CoreBuilder, CoreRuntime, DomainSet,
    GroupMode, Harness, HostKind, ProductIdentity, Provider, ServiceSet, ToolGroups, TrustedAccess,
    TrustedAutomationSource, Workspace,
};

#[test]
fn exposes_the_host_facing_embedding_contract() {
    fn accepts_core(_: Core) {}
    fn accepts_builder(_: CoreBuilder) {}
    fn accepts_runtime(_: Arc<CoreRuntime>) {}
    fn accepts_harness(_: Harness) {}
    fn accepts_access(_: Access) {}
    fn accepts_provider(_: Provider) {}
    fn accepts_workspace(_: Workspace) {}
    fn applies_turn_origin<'a>(
        turn: openhuman_embed::Turn<'a>,
        origin: AgentTurnOrigin,
    ) -> openhuman_embed::Turn<'a> {
        turn.origin(origin)
    }

    let _ = accepts_core;
    let _ = accepts_builder;
    let _ = accepts_runtime;
    let _ = accepts_harness;
    let _ = accepts_access;
    let _ = accepts_provider;
    let _ = accepts_workspace;
    let _ = applies_turn_origin;
    let _ = DomainSet::embedded;
    let _ = ServiceSet::none;
    let _ = HostKind::Library;
    let _ = ToolGroups::none().with("documents", GroupMode::Advertised);
    let automation = AgentTurnOrigin::TrustedAutomation {
        job_id: "embed-public-api".to_string(),
        source: TrustedAutomationSource::Cron,
    };
    let _ = Access::full()
        .trust("/tmp/embed-public-api", TrustedAccess::ReadWrite)
        .origin(automation);
    let _ = set_product_identity;
    assert!(ProductIdentity::new("opencompany").is_some());
}
