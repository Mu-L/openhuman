// The tokenizer's own tests live with it, in `util::bm25`.
    use super::*;

    fn spec(name: &str, description: &str) -> ToolSpec {
        ToolSpec {
            name: name.to_string(),
            description: description.to_string(),
            parameters: json!({"type": "object"}),
        }
    }

    fn index() -> ToolSearchIndex {
        ToolSearchIndex::build(&[
            spec("stock_quote", "Get the latest price for a stock ticker"),
            spec("cron_add", "Schedule a recurring job to run later"),
            spec(
                "memory_hybrid_search",
                "Search stored memories semantically",
            ),
            spec(
                "generate_presentation",
                "Build a pptx slide deck from an outline",
            ),
        ])
    }

    #[test]
    fn a_plain_language_query_finds_the_right_tool() {
        let index = index();
        let hits = index.search("schedule something to run every morning", 3);
        assert_eq!(hits.first().map(|t| t.name.as_str()), Some("cron_add"));
    }

    #[test]
    fn a_query_matching_the_name_rather_than_the_description_still_hits() {
        let index = index();
        let hits = index.search("stock", 3);
        assert_eq!(hits.first().map(|t| t.name.as_str()), Some("stock_quote"));
    }

    #[test]
    fn nothing_relevant_returns_nothing_rather_than_padding_to_the_limit() {
        // Padding would spend exactly the tokens deferral saves, and would
        // invite a call to something unrelated to the ask.
        let index = index();
        assert!(index.search("xyzzy quantum flux", 5).is_empty());
    }

    #[test]
    fn results_are_capped_at_the_requested_limit() {
        let index = index();
        assert!(index.search("search a stock job memory slide", 2).len() <= 2);
    }

    #[test]
    fn an_empty_query_matches_nothing() {
        let index = index();
        assert!(index.search("   ", 5).is_empty());
    }

    #[test]
    fn an_empty_index_is_searchable_without_panicking() {
        // `average_length` is 0 here; the length-normalisation term divides by
        // it, so this is the case that would panic or produce NaN if the
        // `.max(1.0)` guard were dropped.
        let empty = ToolSearchIndex::build(&[]);
        assert!(empty.is_empty());
        assert!(empty.search("anything", 5).is_empty());
    }

    #[test]
    fn ranking_is_stable_across_identical_queries() {
        let index = index();
        let first: Vec<&str> = index
            .search("search", 4)
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        let second: Vec<&str> = index
            .search("search", 4)
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(first, second);
    }
}

/// Remove every [`ToolExposure::Deferred`] and [`ToolExposure::Hidden`] tool
/// from an agent's advertised set, returning the specs of the deferred ones so
/// the caller can index them.
///
/// Hidden tools are dropped and **not** returned: they are not searchable
/// either, by definition.
///
/// `tool_search` itself is never removed, whatever it declares. A search tool
/// the model cannot see is the one failure mode this whole mechanism cannot
/// recover from — every deferred capability would be unreachable, silently.
pub fn strip_deferred_from_visible(
    visible: &mut std::collections::HashSet<String>,
    tools: &[Box<dyn Tool>],
) -> Vec<ToolSpec> {
    use crate::openhuman::tools::ToolExposure;

    let mut deferred = Vec::new();
    for tool in tools {
        let name = tool.name();
        if name == TOOL_SEARCH_NAME || !visible.contains(name) {
            continue;
        }
        match tool.exposure() {
            ToolExposure::Direct => {}
            ToolExposure::Deferred => {
                visible.remove(name);
                deferred.push(tool.spec());
            }
            ToolExposure::Hidden => {
                visible.remove(name);
            }
        }
    }
    deferred
}

/// The advertised name of [`ToolSearchTool`], as a constant so the carve-out
/// above and the registration site cannot disagree about it.
pub const TOOL_SEARCH_NAME: &str = "tool_search";

/// Populate the registry's `tool_search` index with the deferred specs.
///
/// Returns `false` when the registry has no `tool_search` — which is not an
/// error: an agent whose belt defers nothing does not need one, and a build
/// with the tool compiled out is a legitimate configuration. It **is** worth a
/// warning when specs were deferred and there is nowhere to index them, because
/// that combination makes capabilities unreachable.
pub fn bind_tool_search_index(tools: &[Box<dyn Tool>], deferred: Vec<ToolSpec>) -> bool {
    let handle = tools.iter().find_map(|tool| {
        if tool.name() != TOOL_SEARCH_NAME {
            return None;
        }
        tool.host_extension()
            .and_then(|any| any.downcast_ref::<ToolSearchHandle>())
    });
    let Some(handle) = handle else {
        if !deferred.is_empty() {
            tracing::warn!(
                deferred = deferred.len(),
                "[tool_search] tools were deferred but no tool_search is registered; \
                 they are unreachable this session"
            );
        }
        return false;
    };
    match handle.write() {
        Ok(mut index) => {
            *index = ToolSearchIndex::build(&deferred);
            true
        }
        Err(_) => {
            tracing::error!("[tool_search] index lock poisoned; leaving it empty");
            false
        }
    }

