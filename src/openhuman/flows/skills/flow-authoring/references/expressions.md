# Expressions and filters

Use `=` when a value is taken from the current item or an earlier node. Use
`=item` for the current item and `=nodes.<id>.item.json.<field>` for a named
upstream result. Keep natural-language prompts as plain text; do not prefix
them with `=`. Verify bindings with `dry_run_workflow` before proposing a flow.

For list transformations, use the jq filter syntax accepted by the code node
and keep the result an array of items. Preserve the `data` wrapper on
Composio results when addressing their fields.
