impl ToolPolicyMiddleware {
    /// The channel-permission gate the engine ran before the builder policy: a
    /// session-level deny, then a per-call permission-level ceiling check. Returns
    /// the blocking message when the call must not execute.
    fn channel_permission_block(&self, call: &TaToolCall) -> Option<String> {
        let decision = self.session.decision_for(&call.name);
        if decision.is_denied() {
            return Some(
                PolicyDenial::SessionForbidden {
                    tool: &call.name,
                    required: decision.required_permission,
                    allowed: decision.allowed_permission,
                    channel: &self.channel,
                }
                .render(),
            );
        }
        let tool = self.resolve_tool(&call.name)?;
        let call_required = tool.permission_level_with_args(&call.arguments);
        if call_required > decision.allowed_permission {
            return Some(
                PolicyDenial::PermissionTooLow {
                    tool: &call.name,
                    required: call_required,
                    allowed: decision.allowed_permission,
                    channel: &self.channel,
                }
                .render(),
            );
        }
        // For `use_skill`, also validate the resolved inner tool against the
        // session allowlist. Role-hidden packed tools are not checked by the
        // outer policy name; without this check `use_skill` would bypass the
        // session's effective allowlist for any packed tool.
        if call.name == "use_skill" {
            if let Some(inner_tool) = call
                .arguments
                .get("tool")
                .and_then(serde_json::Value::as_str)
            {
                // `blocks_execution`, NOT `is_denied`. Every withheld packed
                // tool is `HideFromPrompt`, and `use_skill` is the only route it
                // has — gating that route on `is_denied` refused all of them.
                let inner_decision = self.session.decision_for(inner_tool);
                if inner_decision.blocks_execution() {
                    // Name the route. A bare denial gives the model nothing to
                    // do differently, and a model with no next step retries the
                    // same call: one live turn burned its whole budget on six
                    // identical `use_skill` denials and died on the
                    // repeated-failure breaker. Same sentence the listing uses,
                    // resolved against the same session, so the two cannot
                    // tell the model different stories.
                    let hint = crate::openhuman::tools::toolpacks::pack_for_tool(inner_tool)
                        .map(|pack| self.route_for_pack(pack))
                        .filter(|h| !h.is_empty())
                        .map(|h| format!(" {h}"))
                        .unwrap_or_default();
                    return Some(format!(
                        "Tool `{inner_tool}` is not allowed in the current session and cannot be used through `use_skill`.{hint}"
                    ));
                }
            }
        }
        None
    }
}
