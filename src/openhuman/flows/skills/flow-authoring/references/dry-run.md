# Reading a dry run

A clean dry run verifies the graph and bindings that the sandbox can inspect;
it does not execute external side effects or prove that a provider will accept
every live value. Treat `null_resolutions`, rejected contracts, and agent
prompt nulls as unfinished work. An `unverifiable` binding is an external
provider limitation: confirm its field with `get_tool_contract` and report the
limitation honestly.
