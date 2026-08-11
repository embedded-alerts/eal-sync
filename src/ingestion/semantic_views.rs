use super::{IngestionError, extract_text};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use url::Url;

const MAX_INPUTS: usize = 96;
const MAX_INPUT_CHARS: usize = 700;
const MAX_EMBEDDING_TEXT_CHARS: usize = 24_000;
const STOPWORDS: &str = concat!(
    "a about after again all also am an and any are as at be because been before being between ",
    "both but by can could did do does doing down during each few for from further had has have ",
    "having he her here hers herself him himself his how i if in into is it its itself just me ",
    "more most my myself no nor not now of off on once only or other our ours ourselves out over ",
    "own same she should so some such than that the their theirs them themselves then there these ",
    "they this those through to too under until up very was we were what when where which while who ",
    "whom why will with would you your yours yourself yourselves",
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingInputKind {
    Title,
    Heading,
    Summary,
    Sentence,
    Entity,
    Keyword,
    UrlSignal,
    Document,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingInput {
    pub kind: EmbeddingInputKind,
    pub ordinal: u16,
    pub text: String,
    pub weight: f32,
}

impl EmbeddingInput {
    pub fn validate(&self) -> Result<(), IngestionError> {
        if self.text.trim().is_empty() || self.text.chars().count() > MAX_INPUT_CHARS {
            return Err(IngestionError::new(
                "invalid_embedding_input",
                format!("embedding input text must contain 1 to {MAX_INPUT_CHARS} characters"),
            ));
        }
        if !self.weight.is_finite() || !(0.1..=2.0).contains(&self.weight) {
            return Err(IngestionError::new(
                "invalid_embedding_input",
                "embedding input weight must be finite and between 0.1 and 2.0",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticDocument {
    pub title: Option<String>,
    pub content_text: String,
    pub keywords: Vec<String>,
    pub entities: Vec<String>,
    pub embedding_inputs: Vec<EmbeddingInput>,
}

impl SemanticDocument {
    pub fn validate(&self) -> Result<(), IngestionError> {
        if self.content_text.trim().is_empty() {
            return Err(IngestionError::new(
                "empty_content",
                "response did not contain extractable text",
            ));
        }
        if self.embedding_inputs.is_empty() || self.embedding_inputs.len() > MAX_INPUTS {
            return Err(IngestionError::new(
                "invalid_embedding_inputs",
                format!("a semantic document must contain 1 to {MAX_INPUTS} embedding inputs"),
            ));
        }
        for input in &self.embedding_inputs {
            input.validate()?;
        }
        Ok(())
    }

    /// Returns deterministic input for providers that still emit one vector per page.
    /// Multi-vector workers should embed `embedding_inputs` independently instead.
    pub fn combined_embedding_text(&self) -> String {
        let mut output = String::new();
        for input in &self.embedding_inputs {
            let label = input_kind_label(input.kind);
            let line = format!("{label}: {}\n", input.text);
            if output.chars().count() + line.chars().count() > MAX_EMBEDDING_TEXT_CHARS {
                break;
            }
            output.push_str(&line);
        }
        output.trim_end().to_owned()
    }
}

pub fn extract_semantic_document(
    content_type: &str,
    body: &[u8],
    canonical_url: &str,
) -> Result<SemanticDocument, IngestionError> {
    let content_text = extract_text(content_type, body)?;
    if content_text.trim().is_empty() {
        return Err(IngestionError::new(
            "empty_content",
            "response did not contain extractable text",
        ));
    }

    let base_type = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim();
    let html = matches!(base_type, "text/html" | "application/xhtml+xml")
        .then(|| String::from_utf8_lossy(body).into_owned());
    let title = html
        .as_deref()
        .and_then(|markup| extract_tag_texts(markup, "title", 1).into_iter().next());
    let mut headings = Vec::new();
    if let Some(markup) = html.as_deref() {
        for tag in ["h1", "h2", "h3"] {
            headings.extend(extract_tag_texts(markup, tag, 12));
        }
    }
    headings = dedupe_text(headings, 24);

    let sentences = complete_sentences(&content_text, 48);
    let summary = build_summary(&sentences, &content_text);
    let keywords = extract_keywords(&content_text, 32);
    let entities = extract_entities(&content_text, 32);

    let mut inputs = Vec::new();
    if let Some(title) = title.as_deref() {
        push_input(&mut inputs, EmbeddingInputKind::Title, title, 1.25);
    }
    for heading in &headings {
        push_input(&mut inputs, EmbeddingInputKind::Heading, heading, 1.15);
    }
    push_input(&mut inputs, EmbeddingInputKind::Summary, &summary, 1.10);
    for group in entities.chunks(6) {
        push_input(
            &mut inputs,
            EmbeddingInputKind::Entity,
            &group.join("; "),
            1.05,
        );
    }
    for group in keywords.chunks(8) {
        push_input(
            &mut inputs,
            EmbeddingInputKind::Keyword,
            &group.join(", "),
            0.90,
        );
    }
    for sentence in &sentences {
        push_input(&mut inputs, EmbeddingInputKind::Sentence, sentence, 1.00);
    }
    if let Some(signal) = url_signal(canonical_url) {
        push_input(&mut inputs, EmbeddingInputKind::UrlSignal, &signal, 0.65);
    }
    push_input(
        &mut inputs,
        EmbeddingInputKind::Document,
        &truncate_chars(&content_text, 2_000),
        0.75,
    );

    dedupe_inputs(&mut inputs);
    inputs.truncate(MAX_INPUTS);
    for (ordinal, input) in inputs.iter_mut().enumerate() {
        input.ordinal = u16::try_from(ordinal).map_err(|_| {
            IngestionError::new("invalid_embedding_inputs", "embedding ordinal overflow")
        })?;
    }

    let document = SemanticDocument {
        title,
        content_text,
        keywords,
        entities,
        embedding_inputs: inputs,
    };
    document.validate()?;
    Ok(document)
}

fn input_kind_label(kind: EmbeddingInputKind) -> &'static str {
    match kind {
        EmbeddingInputKind::Title => "title",
        EmbeddingInputKind::Heading => "heading",
        EmbeddingInputKind::Summary => "summary",
        EmbeddingInputKind::Sentence => "sentence",
        EmbeddingInputKind::Entity => "entities",
        EmbeddingInputKind::Keyword => "keywords",
        EmbeddingInputKind::UrlSignal => "url",
        EmbeddingInputKind::Document => "document",
    }
}

fn push_input(inputs: &mut Vec<EmbeddingInput>, kind: EmbeddingInputKind, text: &str, weight: f32) {
    let text = truncate_chars(&collapse_whitespace(text), MAX_INPUT_CHARS);
    if text.chars().count() < 3 {
        return;
    }
    inputs.push(EmbeddingInput {
        kind,
        ordinal: 0,
        text,
        weight,
    });
}

fn dedupe_inputs(inputs: &mut Vec<EmbeddingInput>) {
    let mut seen = HashSet::new();
    inputs.retain(|input| seen.insert(input.text.to_ascii_lowercase()));
}

fn extract_tag_texts(html: &str, tag: &str, limit: usize) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let open_prefix = format!("<{tag}");
    let close_tag = format!("</{tag}>");
    let mut cursor = 0;
    let mut result = Vec::new();

    while result.len() < limit {
        let Some(open_relative) = lower[cursor..].find(&open_prefix) else {
            break;
        };
        let open = cursor + open_relative;
        let boundary = lower.as_bytes().get(open + open_prefix.len()).copied();
        if !matches!(
            boundary,
            Some(b'>') | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n')
        ) {
            cursor = open + open_prefix.len();
            continue;
        }
        let Some(open_end_relative) = lower[open..].find('>') else {
            break;
        };
        let content_start = open + open_end_relative + 1;
        let Some(close_relative) = lower[content_start..].find(&close_tag) else {
            break;
        };
        let content_end = content_start + close_relative;
        let text = collapse_whitespace(&decode_common_entities(&strip_markup(
            &html[content_start..content_end],
        )));
        if !text.is_empty() {
            result.push(truncate_chars(&text, MAX_INPUT_CHARS));
        }
        cursor = content_end + close_tag.len();
    }
    result
}

fn strip_markup(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut inside_tag = false;
    for character in input.chars() {
        match character {
            '<' => {
                inside_tag = true;
                output.push(' ');
            }
            '>' => {
                inside_tag = false;
                output.push(' ');
            }
            _ if !inside_tag => output.push(character),
            _ => {}
        }
    }
    output
}

fn decode_common_entities(input: &str) -> String {
    input
        .replace("&nbsp;", " ")
        .replace("&#160;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

fn collapse_whitespace(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut pending_space = false;
    for character in input.chars() {
        if character.is_whitespace() {
            pending_space = !output.is_empty();
        } else {
            if pending_space {
                output.push(' ');
            }
            output.push(character);
            pending_space = false;
        }
    }
    output.trim().to_owned()
}

fn complete_sentences(text: &str, limit: usize) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut buffer = String::new();
    for character in text.chars() {
        buffer.push(character);
        if matches!(character, '.' | '!' | '?') {
            let candidate = collapse_whitespace(&buffer);
            let words = candidate.split_whitespace().count();
            let characters = candidate.chars().count();
            if (6..=110).contains(&words) && (36..=900).contains(&characters) {
                sentences.push(candidate);
                if sentences.len() == limit {
                    break;
                }
            }
            buffer.clear();
        } else if buffer.chars().count() > 1_000 {
            buffer.clear();
        }
    }
    dedupe_text(sentences, limit)
}

fn build_summary(sentences: &[String], content_text: &str) -> String {
    let summary = sentences
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if summary.chars().count() >= 80 {
        truncate_chars(&summary, 900)
    } else {
        truncate_chars(content_text, 900)
    }
}

fn normalized_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() || (character == '\'' && !current.is_empty()) {
            current.extend(character.to_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn extract_keywords(text: &str, limit: usize) -> Vec<String> {
    let mut counts = BTreeMap::<String, usize>::new();
    for token in normalized_tokens(text) {
        if token.chars().count() < 3
            || is_stopword(&token)
            || token.chars().all(|character| character.is_numeric())
        {
            continue;
        }
        *counts.entry(token).or_default() += 1;
    }
    let mut ranked = counts.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|(left_word, left_count), (right_word, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_word.cmp(right_word))
    });
    ranked
        .into_iter()
        .take(limit)
        .map(|(word, _)| word)
        .collect()
}

fn extract_entities(text: &str, limit: usize) -> Vec<String> {
    let mut counts = BTreeMap::<String, usize>::new();
    let mut phrase = Vec::<String>::new();

    for raw in text.split_whitespace() {
        let token = raw.trim_matches(|character: char| {
            !character.is_alphanumeric() && character != '-' && character != '\''
        });
        if is_entity_token(token) {
            phrase.push(token.to_owned());
            if phrase.len() == 5 {
                flush_entity(&mut phrase, &mut counts);
            }
        } else {
            flush_entity(&mut phrase, &mut counts);
        }
    }
    flush_entity(&mut phrase, &mut counts);

    let mut ranked = counts.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|(left_name, left_count), (right_name, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_name.cmp(right_name))
    });
    ranked
        .into_iter()
        .take(limit)
        .map(|(name, _)| name)
        .collect()
}

fn flush_entity(phrase: &mut Vec<String>, counts: &mut BTreeMap<String, usize>) {
    if phrase.is_empty() {
        return;
    }
    let candidate = phrase.join(" ");
    if candidate.chars().count() >= 3 && !is_common_sentence_lead(&candidate) {
        *counts.entry(candidate).or_default() += 1;
    }
    phrase.clear();
}

fn is_entity_token(token: &str) -> bool {
    if token.chars().count() < 2 || token.chars().all(|character| character.is_numeric()) {
        return false;
    }
    let Some(first) = token.chars().next() else {
        return false;
    };
    if !first.is_uppercase() {
        return false;
    }
    let letters = token
        .chars()
        .filter(|character| character.is_alphabetic())
        .collect::<Vec<_>>();
    let uppercase = letters
        .iter()
        .filter(|character| character.is_uppercase())
        .count();
    uppercase == 1 || (uppercase == letters.len() && letters.len() <= 8)
}

fn is_common_sentence_lead(candidate: &str) -> bool {
    matches!(
        candidate,
        "A" | "An"
            | "And"
            | "But"
            | "For"
            | "How"
            | "In"
            | "It"
            | "On"
            | "Or"
            | "The"
            | "This"
            | "To"
            | "We"
            | "What"
            | "When"
            | "Where"
            | "Why"
            | "You"
    )
}

fn is_stopword(token: &str) -> bool {
    STOPWORDS
        .split_ascii_whitespace()
        .any(|stopword| stopword == token)
}

fn url_signal(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    let mut terms = Vec::new();
    if let Some(host) = url.host_str() {
        terms.extend(
            host.split('.')
                .filter(|part| part.len() > 2 && !matches!(*part, "www" | "com" | "org" | "net"))
                .map(str::to_owned),
        );
    }
    terms.extend(
        url.path_segments()
            .into_iter()
            .flatten()
            .flat_map(|segment| segment.split(['-', '_']))
            .filter(|part| part.len() > 2)
            .map(str::to_owned),
    );
    let terms = terms.into_iter().collect::<BTreeSet<_>>();
    (!terms.is_empty()).then(|| terms.into_iter().collect::<Vec<_>>().join(" "))
}

fn dedupe_text(items: Vec<String>, limit: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.to_ascii_lowercase()))
        .take(limit)
        .collect()
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let mut output = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_title_entities_keywords_and_complete_sentences() {
        let html = br#"
            <html>
              <head>
                <title>Acme Launches Atlas</title>
                <script>SecretNoise SecretNoise</script>
              </head>
              <body>
                <h1>Acme Corporation launches Atlas in Bogota</h1>
                <p>Acme Corporation announced that Atlas will monitor renewable energy projects across Colombia.</p>
                <p>The platform helps engineering teams detect project risks before schedules begin to slip.</p>
              </body>
            </html>
        "#;
        let document = extract_semantic_document(
            "text/html; charset=utf-8",
            html,
            "https://example.com/news/acme-atlas",
        )
        .unwrap();

        assert_eq!(document.title.as_deref(), Some("Acme Launches Atlas"));
        assert!(document.keywords.iter().any(|keyword| keyword == "atlas"));
        assert!(
            document
                .entities
                .iter()
                .any(|entity| entity.contains("Acme Corporation"))
        );
        assert!(document.embedding_inputs.iter().any(|input| {
            input.kind == EmbeddingInputKind::Sentence
                && input.text.contains("renewable energy projects")
        }));
        assert!(
            document
                .embedding_inputs
                .iter()
                .any(|input| input.kind == EmbeddingInputKind::Keyword)
        );
        assert!(
            document
                .embedding_inputs
                .iter()
                .any(|input| input.kind == EmbeddingInputKind::Entity)
        );
        assert!(!document.content_text.contains("SecretNoise"));
        assert!(document.combined_embedding_text().contains("keywords:"));
    }

    #[test]
    fn non_html_documents_still_get_bounded_semantic_views() {
        let document = extract_semantic_document(
            "application/json",
            br#"{"title":"Solar launch","summary":"A renewable energy platform launched in Colombia for engineering teams."}"#,
            "https://example.com/api/solar-launch",
        )
        .unwrap();
        assert!(!document.embedding_inputs.is_empty());
        assert!(document.embedding_inputs.len() <= MAX_INPUTS);
        assert!(
            document
                .embedding_inputs
                .iter()
                .all(|input| input.text.chars().count() <= MAX_INPUT_CHARS)
        );
    }

    #[test]
    fn embedding_input_validation_rejects_non_finite_weights() {
        let input = EmbeddingInput {
            kind: EmbeddingInputKind::Summary,
            ordinal: 0,
            text: "valid text".into(),
            weight: f32::NAN,
        };
        assert!(input.validate().is_err());
    }

    #[test]
    fn stopwords_filter_common_terms() {
        assert!(is_stopword("the"));
        assert!(!is_stopword("renewable"));
    }
}
