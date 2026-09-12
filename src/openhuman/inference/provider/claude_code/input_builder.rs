//! Build the stream-json stdin payload fed to `claude --input-format stream-json`.
//!
//! The CLI consumes one JSON object per line on stdin. Each line looks
//! like:
//!   { "type":"user", "message":{"role":"user","content":[{"type":"text","text":"..."}]} }
//!
//! v1 piping policy:
//! - On a *new* CC session: send every history `ChatMessage` so claude
//!   has full context (system message is conveyed via
//!   `--append-system-prompt`, not stdin).
//! - On a `--resume` of an existing CC session: claude already has prior
//!   turns server-side; we only send the last user turn.

use base64::Engine as _;
use serde_json::{json, Value};

use crate::openhuman::agent::messages::ChatMessage;
use crate::openhuman::agent::multimodal::{
    is_managed_attachment_path, parse_image_markers, rehydrate_image_placeholders,
};

/// Build the bytes to write to claude's stdin. Returns an empty `Vec`
/// when there is nothing to send (caller should abort).
pub fn build_stdin(messages: &[ChatMessage], is_new_session: bool) -> Vec<u8> {
    // Resolve any `[Image: … #att:<id>]` placeholders to on-disk `[IMAGE:<path>]`
    // markers so pasted images can be inlined below. No-op for messages that
    // carry no image placeholder, so plain-text turns are unaffected.
    let rehydrated = rehydrate_image_placeholders(messages);
    let messages: &[ChatMessage] = &rehydrated;

    // `--input-format stream-json` accepts ONLY user-role turns — replaying an
    // assistant turn fails with `Expected message role 'user', got 'assistant'`.
    // So we always emit exactly one user message: the trailing user turn. On a
    // *new* session (including a recreated one — see the session_store recovery
    // path) any prior turns are folded into a text preamble so the session keeps
    // its context instead of erroring; on resume the CLI already holds them.
    let non_system: Vec<&ChatMessage> = messages.iter().filter(|m| m.role != "system").collect();
    let Some(last_user_pos) = non_system.iter().rposition(|m| m.role == "user") else {
        return Vec::new();
    };

    let mut content: Vec<Value> = Vec::new();
    if is_new_session {
        if let Some(preamble) = prior_conversation_preamble(&non_system, last_user_pos) {
            content.push(json!({"type": "text", "text": preamble}));
        }
    }
    // Pasted images arrive as inline `[IMAGE:data:…]` markers in the user turn's
    // content (the multimodal pipeline rehydrates them, and the native-provider
    // message bridge re-emits them from its typed image blocks — see
    // `message_convert::message_to_native_chat_message`). `content_blocks` splits
    // each marker into a real Anthropic `image` block below.
    content.extend(content_blocks(&non_system[last_user_pos].content));
    if content.is_empty() {
        return Vec::new();
    }

    let line = json!({
        "type": "user",
        "message": { "role": "user", "content": content },
    });
    let mut out = String::new();
    push_json_line(&mut out, &line);
    out.into_bytes()
}

/// Render the turns before `end` (the latest user turn) as a plain-text preamble
/// so a newly-created CC session inherits the thread's context — the CLI can't
/// accept assistant-role input, so this is the only way to seed prior turns.
///
/// A user turn with no assistant reply after it is skipped: that is a failed or
/// pending attempt (e.g. a message that errored and was then re-asked), not part
/// of the conversation. Replaying it would duplicate the re-asked current turn
/// and make the message look like it "came through twice". Image markers are
/// stripped (only the latest turn re-sends its images). Returns `None` when
/// there is nothing to carry over (a genuinely fresh conversation).
fn prior_conversation_preamble(non_system: &[&ChatMessage], end: usize) -> Option<String> {
    let mut turns = Vec::new();
    for (i, m) in non_system.iter().take(end).enumerate() {
        match m.role.as_str() {
            "assistant" => {
                let (text, _images) = parse_image_markers(&m.content);
                if !text.is_empty() {
                    turns.push(format!("Assistant: {text}"));
                }
            }
            "user" => {
                // Only fold an *answered* user turn (an assistant reply follows).
                let answered = non_system
                    .get(i + 1)
                    .is_some_and(|next| next.role == "assistant");
                if answered {
                    let (text, _images) = parse_image_markers(&m.content);
                    if !text.is_empty() {
                        turns.push(format!("User: {text}"));
                    }
                }
            }
            _ => {}
        }
    }
    if turns.is_empty() {
        return None;
    }
    Some(format!(
        "[Earlier in this conversation]\n{}\n[End of earlier conversation]\n",
        turns.join("\n")
    ))
}

/// Split a message's text into stream-json content blocks: the prose as a
/// `text` block, plus one native `image` block per `[IMAGE:<ref>]` marker (the
/// `claude` CLI + Opus are vision-capable). An image that cannot be read
/// degrades to a short text note rather than being silently dropped.
fn content_blocks(raw: &str) -> Vec<Value> {
    let mut blocks: Vec<Value> = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = raw[cursor..].find("[IMAGE:") {
        let start = cursor + relative;
        let Some(end_relative) = raw[start..].find(']') else {
            break;
        };
        let end = start + end_relative + 1;
        if start > cursor {
            blocks.push(json!({"type": "text", "text": &raw[cursor..start]}));
        }
        let reference = &raw[start + "[IMAGE:".len()..end - 1];
        match image_block(reference) {
            Some(block) => blocks.push(block),
            None => blocks
                .push(json!({"type": "text", "text": "[an attached image could not be read]"})),
        }
        cursor = end;
    }
    if cursor < raw.len() {
        blocks.push(json!({"type": "text", "text": &raw[cursor..]}));
    }
    if blocks.is_empty() {
        // Preserve prior behaviour for a genuinely empty message.
        blocks.push(json!({"type": "text", "text": raw}));
    }
    blocks
}

/// Build an Anthropic `image` content block from an `[IMAGE:<ref>]` reference.
/// `<ref>` is either a `data:` URI (inline base64) or an on-disk file path (a
/// rehydrated attachment). Returns `None` when the ref cannot be resolved.
fn image_block(reference: &str) -> Option<Value> {
    let (media_type, data_b64) = if let Some(rest) = reference.strip_prefix("data:") {
        let (mime, data) = rest.split_once(";base64,")?;
        (mime.to_string(), data.to_string())
    } else {
        if !is_managed_attachment_path(reference) {
            return None;
        }
        let bytes = std::fs::read(reference).ok()?;
        (
            media_type_from_path(reference),
            base64::engine::general_purpose::STANDARD.encode(bytes),
        )
    };
    Some(json!({
        "type": "image",
        "source": {"type": "base64", "media_type": media_type, "data": data_b64},
    }))
}

/// Best-effort media type from a file extension. Claude accepts jpeg/png/gif/webp.
fn media_type_from_path(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else {
        "image/jpeg"
    }
    .to_string()
}

fn push_json_line(buf: &mut String, v: &Value) {
    buf.push_str(&serde_json::to_string(v).unwrap_or_default());
    buf.push('\n');
}

#[cfg(test)]
#[path = "input_builder_tests.rs"]
mod tests;
