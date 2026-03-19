//! Output redaction — scrub secret values from tool results.
//!
//! Replaces exact matches with `[REDACTED:NAME]` markers. Handles partial
//! matches (secrets appearing in larger strings like URLs with embedded tokens).

use std::collections::HashMap;

/// Redact all known secret values from a string.
///
/// Longer secrets are replaced first to avoid partial-match issues
/// (e.g., a token that contains a shorter key as a substring).
pub fn redact_string(input: &str, secrets: &HashMap<String, String>) -> String {
    if secrets.is_empty() || input.is_empty() {
        return input.to_string();
    }

    // Sort by value length descending — replace longest matches first
    let mut sorted: Vec<(&String, &String)> = secrets.iter().collect();
    sorted.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    let mut result = input.to_string();
    for (name, value) in sorted {
        // Skip very short values (< 8 chars) to avoid false positives
        if value.len() < 8 {
            continue;
        }
        if result.contains(value.as_str()) {
            result = result.replace(value.as_str(), &format!("[REDACTED:{name}]"));
        }
    }
    result
}

/// Redact secrets from content blocks (text blocks only).
pub fn redact_content_blocks(
    content: &mut Vec<omegon_traits::ContentBlock>,
    secrets: &HashMap<String, String>,
) {
    if secrets.is_empty() {
        return;
    }
    for block in content.iter_mut() {
        if let omegon_traits::ContentBlock::Text { text } = block {
            *text = redact_string(text, secrets);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_single_secret() {
        let mut secrets = HashMap::new();
        secrets.insert("API_KEY".into(), "sk-ant-api03-very-secret-key".into());
        let input = "Authorization: Bearer sk-ant-api03-very-secret-key";
        let result = redact_string(input, &secrets);
        assert_eq!(result, "Authorization: Bearer [REDACTED:API_KEY]");
    }

    #[test]
    fn redact_multiple_secrets() {
        let mut secrets = HashMap::new();
        secrets.insert("TOKEN_A".into(), "aaaa-bbbb-cccc-dddd".into());
        secrets.insert("TOKEN_B".into(), "xxxx-yyyy-zzzz-1234".into());
        let input = "a=aaaa-bbbb-cccc-dddd b=xxxx-yyyy-zzzz-1234";
        let result = redact_string(input, &secrets);
        assert_eq!(result, "a=[REDACTED:TOKEN_A] b=[REDACTED:TOKEN_B]");
    }

    #[test]
    fn skip_short_values() {
        let mut secrets = HashMap::new();
        secrets.insert("SHORT".into(), "abc".into()); // < 8 chars
        let input = "the abc value should not be redacted";
        let result = redact_string(input, &secrets);
        assert_eq!(result, input); // unchanged
    }

    #[test]
    fn empty_input_returns_empty() {
        let secrets = HashMap::new();
        assert_eq!(redact_string("", &secrets), "");
    }

    #[test]
    fn no_secrets_returns_input() {
        let secrets = HashMap::new();
        let input = "nothing to redact";
        assert_eq!(redact_string(input, &secrets), input);
    }

    #[test]
    fn longest_match_first() {
        let mut secrets = HashMap::new();
        secrets.insert("FULL".into(), "sk-ant-api03-full-key-here".into());
        secrets.insert("PREFIX".into(), "sk-ant-api03".into());
        let input = "key is sk-ant-api03-full-key-here done";
        let result = redact_string(input, &secrets);
        assert_eq!(result, "key is [REDACTED:FULL] done");
    }
}
