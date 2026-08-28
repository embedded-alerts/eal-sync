//! Bounded, redirect-safe ingestion primitives for registered sources.

mod content;
mod network;
mod revision;
mod semantic_views;

pub use content::{DiscoveredLink, discover_links, extract_text};
pub use network::{
    ConditionalState, FetchMetadata, FetchOutcome, FetchPolicy, FetchRequest, FetchScope,
    FetchedDocument, HttpFetcher, is_public_ip,
};
pub use revision::{
    EmbeddingWorkItem, RevisionDecision, decide_revision, decide_revision_with_inputs,
};
pub use semantic_views::{
    EmbeddingInput, EmbeddingInputKind, SemanticDocument, extract_semantic_document,
};

use serde::Serialize;
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IngestionError {
    pub code: &'static str,
    pub message: String,
}

impl IngestionError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for IngestionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for IngestionError {}
