use super::IngestionError;
use eal_semantic::canonicalize_url;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use url::Url;

pub fn extract_text(content_type: &str, body: &[u8]) -> Result<String, IngestionError> {
    let base = base_content_type(content_type);
    let decoded = String::from_utf8_lossy(body);
    let text = if base == "application/json"
        || base == "application/feed+json"
        || base.ends_with("+json")
    {
        let value: Value = serde_json::from_slice(body).map_err(|error| {
            IngestionError::new("invalid_json", format!("could not parse JSON: {error}"))
        })?;
        let mut fragments = Vec::new();
        collect_json_text(&value, 0, &mut fragments)?;
        fragments.join(" ")
    } else if matches!(base, "text/html" | "application/xhtml+xml") {
        strip_markup(&remove_ignored_html_blocks(&decoded))
    } else if base == "text/plain" {
        decoded.into_owned()
    } else {
        strip_markup(&decoded)
    };
    Ok(collapse_whitespace(&decode_common_entities(&text)))
}

fn base_content_type(value: &str) -> &str {
    value.split(';').next().unwrap_or(value).trim()
}

fn collect_json_text(
    value: &Value,
    depth: usize,
    output: &mut Vec<String>,
) -> Result<(), IngestionError> {
    if depth > 64 {
        return Err(IngestionError::new(
            "json_too_deep",
            "JSON nesting exceeds 64 levels",
        ));
    }
    if output.len() >= 20_000 {
        return Err(IngestionError::new(
            "json_too_many_strings",
            "JSON contains more than 20000 text fragments",
        ));
    }
    match value {
        Value::String(value) => output.push(value.clone()),
        Value::Array(values) => {
            for value in values {
                collect_json_text(value, depth + 1, output)?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                output.push(key.clone());
                collect_json_text(value, depth + 1, output)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
    Ok(())
}

fn remove_ignored_html_blocks(input: &str) -> String {
    let mut output = input.to_owned();
    for tag in ["script", "style", "noscript", "template", "svg"] {
        loop {
            let lowered = output.to_ascii_lowercase();
            let opening = format!("<{tag}");
            let Some(start) = lowered.find(&opening) else {
                break;
            };
            let closing = format!("</{tag}>");
            let end = lowered[start..]
                .find(&closing)
                .map(|relative| start + relative + closing.len())
                .unwrap_or(output.len());
            output.replace_range(start..end, " ");
        }
    }
    output
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
    output
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveredLink {
    pub url: String,
    pub canonical_url: String,
}

pub fn discover_links(
    base_url: &str,
    html: &str,
    same_host_only: bool,
    max_links: usize,
) -> Result<Vec<DiscoveredLink>, IngestionError> {
    if max_links == 0 || max_links > 10_000 {
        return Err(IngestionError::new(
            "invalid_link_limit",
            "max_links must be between 1 and 10000",
        ));
    }
    let base = parse_http_url(base_url)?;
    let base_host = base
        .host_str()
        .ok_or_else(|| IngestionError::new("invalid_url", "base URL host is required"))?;
    let mut links = BTreeMap::new();
    for href in extract_href_values(html) {
        if href.starts_with('#')
            || href.starts_with("mailto:")
            || href.starts_with("javascript:")
            || href.starts_with("data:")
        {
            continue;
        }
        let Ok(mut candidate) = base.join(&href) else {
            continue;
        };
        candidate.set_fragment(None);
        if !matches!(candidate.scheme(), "http" | "https")
            || !candidate.username().is_empty()
            || candidate.password().is_some()
        {
            continue;
        }
        if same_host_only && candidate.host_str() != Some(base_host) {
            continue;
        }
        let canonical_url = match canonical_identity(&candidate) {
            Ok(value) => value,
            Err(_) => continue,
        };
        links
            .entry(canonical_url.clone())
            .or_insert_with(|| DiscoveredLink {
                url: candidate.as_str().into(),
                canonical_url,
            });
        if links.len() >= max_links {
            break;
        }
    }
    Ok(links.into_values().collect())
}

fn parse_http_url(input: &str) -> Result<Url, IngestionError> {
    let mut url = Url::parse(input.trim()).map_err(|error| {
        IngestionError::new("invalid_url", format!("could not parse URL: {error}"))
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(IngestionError::new(
            "invalid_url",
            "URL scheme must be http or https",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(IngestionError::new(
            "invalid_url",
            "URL user information is not allowed",
        ));
    }
    if url.host_str().is_none() {
        return Err(IngestionError::new("invalid_url", "URL host is required"));
    }
    url.set_fragment(None);
    Ok(url)
}

fn canonical_identity(url: &Url) -> Result<String, IngestionError> {
    canonicalize_url(url.as_str())
        .map(|normalized| normalized.canonical)
        .map_err(|error| IngestionError::new("invalid_url", error.to_string()))
}

fn extract_href_values(html: &str) -> Vec<String> {
    let lowered = html.to_ascii_lowercase();
    let bytes = html.as_bytes();
    let mut values = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = lowered[cursor..].find("href") {
        let mut index = cursor + relative + 4;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if bytes.get(index) != Some(&b'=') {
            cursor = index.min(bytes.len());
            continue;
        }
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        let quote = matches!(bytes[index], b'\'' | b'\"').then_some(bytes[index]);
        if quote.is_some() {
            index += 1;
        }
        let start = index;
        while index < bytes.len()
            && match quote {
                Some(quote) => bytes[index] != quote,
                None => !bytes[index].is_ascii_whitespace() && bytes[index] != b'>',
            }
        {
            index += 1;
        }
        if start < index {
            values.push(String::from_utf8_lossy(&bytes[start..index]).into_owned());
        }
        cursor = index.saturating_add(1).min(bytes.len());
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_visible_html_without_script_or_style_noise() {
        let text = extract_text(
            "text/html; charset=utf-8",
            br#"<html><head><style>.x{}</style><script>alert('x')</script></head><body><h1>New &amp; useful</h1><p>Rust alerts</p></body></html>"#,
        )
        .unwrap();
        assert_eq!(text, "New & useful Rust alerts");
    }

    #[test]
    fn extracts_json_keys_and_string_values() {
        let text = extract_text(
            "application/json",
            br#"{"title":"New page","items":[{"url":"https://example.com/a"}]}"#,
        )
        .unwrap();
        assert_eq!(text, "title New page items url https://example.com/a");
    }

    #[test]
    fn discovers_and_deduplicates_links() {
        let links = discover_links(
            "https://example.com/root/",
            r#"<a href="/a?utm_source=x#frag">one</a><a href="https://example.com/a">two</a><a href="https://other.test/b">other</a>"#,
            true,
            10,
        )
        .unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].canonical_url, "https://example.com/a");
        assert_eq!(links[0].url, "https://example.com/a?utm_source=x");
    }
}
