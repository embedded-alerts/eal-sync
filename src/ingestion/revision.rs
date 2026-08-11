use super::IngestionError;
use eal_semantic::canonicalize_url;
use serde::{Deserialize, Serialize};

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

fn validate_sha256(value: &str) -> Result<(), IngestionError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(IngestionError::new(
            "invalid_content_hash",
            "content hash must be a SHA-256 hexadecimal digest",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingWorkItem {
    pub tenant_id: String,
    pub source_id: String,
    pub canonical_url: String,
    pub source_revision_id: String,
    pub content_sha256: String,
    pub content_text: String,
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
        }
        validate_sha256(&self.content_sha256)?;
        canonicalize_url(&self.canonical_url).map_err(|error| {
            IngestionError::new("invalid_embedding_work_item", error.to_string())
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            decide_revision(Some(previous), current).unwrap(),
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
            embedding_space_id: "space".into(),
        };
        assert!(item.validate().is_err());
    }
}
