//! Session persistence: transcript loading, checkpointing, and background tasks.

use super::super::transcript;
use super::super::transcript_history::{
    FileTranscriptLocator, SessionHistoryLocator, TranscriptTurn,
};
use super::super::types::Agent;
use crate::agent::context::ARCHIVIST_EXTRACTION_PROMPT;
use crate::agent::harness;
use crate::agent::messages::ChatMessage;
use crate::agent::progress::AgentProgress;
use crate::inference::provider::{ChatResponse, UsageInfo, AGENT_TURN_MAX_OUTPUT_TOKENS};
use futures::StreamExt;
use tinyinference::model::{ModelRequest, ModelStreamItem};
include!("session_io_impl_01_part_01.rs");
include!("session_io_impl_01_part_02.rs");
