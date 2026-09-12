# Node configuration

Read `get_node_kind_contract` for the authoritative fields and ports of each
node kind. Memory nodes use an explicit scope; dedup nodes need a stable key;
trigger nodes need a supported trigger kind and its required configuration.

Set error handling deliberately. Required connection and credential values
must be wired from a trusted source or an existing connection, never guessed.
Validate the complete graph after configuration.
