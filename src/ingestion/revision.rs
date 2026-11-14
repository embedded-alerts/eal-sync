use super::{EmbeddingInput, IngestionError};
use eal_semantic::canonicalize_url;
use serde::{Deserialize, Serialize};

const MAX_EMBEDDING_INPUTS: usize = 96;
const MAX_EMBEDDING_TEXT_CHARS: usize = 24_000;
// Matches the maximum permitted fetch response size; also caps externally
// supplied and replayed work items before downstream model processing.
const MAX_CONTENT_TEXT_BYTES: usize = 32 * 1024 * 1024;
// Prevent unbounded metadata fields and unsafe control bytes in durable work
// items. These limits are intentionally larger than normal UUID/space keys.
const MAX_IDENTITY_FIELD_BYTES: usize = 4 * 1024;
const MAX_CANONICAL_URL_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum RevisionDecision {
    Unchanged,
    StoreRevision {
        previous_content_sha256: Option<String>,
        content_sha256: String,
    },
}

pub fn decide_revision(
    previous_content_sha256: Option<&str>,
    current_content_sha256: &str,
) -> Result<RevisionDecision, IngestionError> {
    validate_sha256(current_content_sha256)?;
    if let Some(previous) = previous_content_sha256 {
        validate_sha256(previous)?;
        if previous.eq_ignore_ascii_case(current_content_sha256) {
            return Ok(RevisionDecision::Unchanged);
        }
    }
    Ok(RevisionDecision::StoreRevision {
        previous_content_sha256: previous_content_sha256.map(str::to_ascii_lowercase),
        content_sha256: current_content_sha256.to_ascii_lowercase(),
    })
}

pub fn decide_revision_with_inputs(
    previous_content_sha256: Option<&str>,
    current_content_sha256: &str,
    embedding_inputs: &[EmbeddingInput],
) -> Result<RevisionDecision, IngestionError> {
    validate_embedding_inputs(embedding_inputs)?;
    decide_revision(previous_content_sha256, current_content_sha256)
}

fn validate_sha256(value: &str) -> Result<(), IngestionError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(IngestionError::new(
            "invalid_content_hash",
            "content hash must be a SHA-256 hexadecimal digest",
        ));
    }
    Ok(())
}

fn validate_embedding_inputs(inputs: &[EmbeddingInput]) -> Result<(), IngestionError> {
    if inputs.is_empty() || inputs.len() > MAX_EMBEDDING_INPUTS {
        return Err(IngestionError::new(
            "invalid_embedding_inputs",
            format!("embedding work must contain 1 to {MAX_EMBEDDING_INPUTS} structured inputs"),
        ));
    }
    for (expected_ordinal, input) in inputs.iter().enumerate() {
        input.validate()?;
        if usize::from(input.ordinal) != expected_ordinal {
            return Err(IngestionError::new(
                "invalid_embedding_inputs",
                "embedding input ordinals must be contiguous and start at zero",
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingWorkItem {
    pub tenant_id: String,
    pub source_id: String,
    pub canonical_url: String,
    pub source_revision_id: String,
    pub content_sha256: String,
    pub content_text: String,
    #[serde(default)]
    pub embedding_text: String,
    #[serde(default)]
    pub embedding_inputs: Vec<EmbeddingInput>,
    pub embedding_space_id: String,
}

impl EmbeddingWorkItem {
    pub fn validate(&self) -> Result<(), IngestionError> {
        for (field, value) in [
            ("tenant_id", self.tenant_id.as_str()),
            ("source_id", self.source_id.as_str()),
            ("canonical_url", self.canonical_url.as_str()),
            ("source_revision_id", self.source_revision_id.as_str()),
            ("content_text", self.content_text.as_str()),
            ("embedding_space_id", self.embedding_space_id.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(IngestionError::new(
                    "invalid_embedding_work_item",
                    format!("{field} must not be empty"),
                ));
            }
            let metadata_limit = if field == "canonical_url" {
                MAX_CANONICAL_URL_BYTES
            } else {
                MAX_IDENTITY_FIELD_BYTES
            };
            if field != "content_text"
                && (value.len() > metadata_limit || value.chars().any(char::is_control))
            {
                return Err(IngestionError::new(
                    "invalid_embedding_work_item",
                    format!("{field} exceeds its metadata limits or contains control characters"),
                ));
            }
        }
        if self.content_text.len() > MAX_CONTENT_TEXT_BYTES {
            return Err(IngestionError::new(
                "invalid_embedding_work_item",
                "content_text exceeds the maximum permitted fetch response size",
            ));
        }
        if self.embedding_text.chars().count() > MAX_EMBEDDING_TEXT_CHARS {
            return Err(IngestionError::new(
                "invalid_embedding_work_item",
                format!(
                    "embedding_text must contain at most {MAX_EMBEDDING_TEXT_CHARS} characters"
                ),
            ));
        }
        if !self.embedding_inputs.is_empty() {
            validate_embedding_inputs(&self.embedding_inputs)?;
        }
        validate_sha256(&self.content_sha256)?;
        canonicalize_url(&self.canonical_url).map_err(|error| {
            IngestionError::new("invalid_embedding_work_item", error.to_string())
        })?;
        Ok(())
    }

    pub fn provider_text(&self) -> &str {
        if self.embedding_text.trim().is_empty() {
            &self.content_text
        } else {
            &self.embedding_text
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingestion::EmbeddingInputKind;

    fn input() -> EmbeddingInput {
        EmbeddingInput {
            kind: EmbeddingInputKind::Sentence,
            ordinal: 0,
            text: "A complete sentence about a renewable energy launch.".into(),
            weight: 1.0,
        }
    }

    #[test]
    fn unchanged_content_is_a_no_op() {
        let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        assert_eq!(
            decide_revision(Some(hash), &hash.to_ascii_uppercase()).unwrap(),
            RevisionDecision::Unchanged
        );
    }

    #[test]
    fn changed_content_links_to_the_previous_hash() {
        let previous = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let current = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        assert_eq!(
            decide_revision_with_inputs(Some(previous), current, &[input()]).unwrap(),
            RevisionDecision::StoreRevision {
                previous_content_sha256: Some(previous.into()),
                content_sha256: current.into(),
            }
        );
    }

    #[test]
    fn embedding_work_requires_a_canonicalizable_http_url() {
        let item = EmbeddingWorkItem {
            tenant_id: "tenant".into(),
            source_id: "source".into(),
            canonical_url: "file:///etc/passwd".into(),
            source_revision_id: "revision".into(),
            content_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .into(),
            content_text: "new page".into(),
            embedding_text: "document: new page".into(),
            embedding_inputs: vec![input()],
            embedding_space_id: "space".into(),
        };
        assert!(item.validate().is_err());
    }

    #[test]
    fn embedding_work_rejects_content_larger_than_fetch_policy_ceiling() {
        let item = EmbeddingWorkItem {
            tenant_id: "tenant".into(),
            source_id: "source".into(),
            canonical_url: "https://example.com/news".into(),
            source_revision_id: "revision".into(),
            content_sha256: "a".repeat(64),
            content_text: "x".repeat(MAX_CONTENT_TEXT_BYTES + 1),
            embedding_text: String::new(),
            embedding_inputs: vec![input()],
            embedding_space_id: "space".into(),
        };
        let error = item.validate().unwrap_err();
        assert_eq!(error.code, "invalid_embedding_work_item");
        assert!(!error.message.contains("x".repeat(128).as_str()));
    }

    #[test]
    fn metadata_fields_are_bounded_and_reject_control_characters() {
        fn valid_item() -> EmbeddingWorkItem {
            EmbeddingWorkItem {
                tenant_id: "tenant".into(),
                source_id: "source".into(),
                canonical_url: "https://example.com/news".into(),
                source_revision_id: "revision".into(),
                content_sha256: "a".repeat(64),
                content_text: "a bounded document".into(),
                embedding_text: "a bounded summary".into(),
                embedding_inputs: vec![input()],
                embedding_space_id: "provider:model:1536:provider".into(),
            }
        }
        let mut permitted = valid_item();
        permitted.tenant_id = "t".repeat(MAX_IDENTITY_FIELD_BYTES);
        assert!(permitted.validate().is_ok());

        for invalid_value in [
            "x".repeat(MAX_IDENTITY_FIELD_BYTES + 1),
            "bad\\nname".to_owned(),
            "bad\\rname".to_owned(),
            "bad\\u{0}name".to_owned(),
        ] {
            let mut item = valid_item();
            item.tenant_id = invalid_value.clone();
            let err = item.validate().unwrap_err();
            assert_eq!(err.code, "invalid_embedding_work_item");
            assert!(!err.message.contains(&invalid_value));

            let mut item = valid_item();
            item.source_revision_id = invalid_value;
            assert_eq!(
                item.validate().unwrap_err().code,
                "invalid_embedding_work_item"
            );
        }
        let mut item = valid_item();
        item.canonical_url = format!(
            "https://example.com/{}",
            "x".repeat(MAX_CANONICAL_URL_BYTES)
        );
        assert_eq!(
            item.validate().unwrap_err().code,
            "invalid_embedding_work_item"
        );
        let mut item = valid_item();
        item.embedding_space_id = "bad\\nspace".into();
        assert_eq!(
            item.validate().unwrap_err().code,
            "invalid_embedding_work_item"
        );
    }

    #[test]
    fn legacy_work_items_fall_back_to_full_content_text() {
        let item = EmbeddingWorkItem {
            tenant_id: "tenant".into(),
            source_id: "source".into(),
            canonical_url: "https://example.com/news".into(),
            source_revision_id: "revision".into(),
            content_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .into(),
            content_text: "legacy page text".into(),
            embedding_text: String::new(),
            embedding_inputs: Vec::new(),
            embedding_space_id: "space".into(),
        };
        item.validate().unwrap();
        assert_eq!(item.provider_text(), "legacy page text");
    }

    #[test]
    fn structured_inputs_require_contiguous_ordinals() {
        let mut invalid = input();
        invalid.ordinal = 3;
        assert!(
            decide_revision_with_inputs(
                None,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                &[invalid],
            )
            .is_err()
        );
    }
}
