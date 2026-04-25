//! JSON extraction from LLM output.
//!
//! LLM responses often wrap JSON in prose or markdown fences. This module
//! provides multi-strategy extraction that tries direct parsing, fenced code
//! blocks, and balanced-bracket scanning.

use log::warn;
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

static JSON_FENCE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)```(?:json)?\s*\n?(.*?)\n?\s*```").expect("valid JSON fence regex")
});

/// Try to parse JSON from LLM output using multiple strategies.
///
/// Strategies attempted in order:
/// 1. **Direct parse** — the entire output is valid JSON
/// 2. **Markdown fence** — extract from `` ```json ... ``` `` blocks
/// 3. **Balanced brackets** — find the first `{...}` or `[...]` block
///
/// Returns `None` if all strategies fail.
pub fn parse_json_output(output: &str, step_id: &str) -> Option<Value> {
    let text = output.trim();

    // Strategy 1: Direct parse
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return Some(v);
    }

    // Strategy 2: Extract from markdown fences
    if let Some(caps) = JSON_FENCE_RE.captures(text)
        && let Some(m) = caps.get(1)
        && let Ok(v) = serde_json::from_str::<Value>(m.as_str().trim())
    {
        return Some(v);
    }

    // Strategy 3: Find first balanced JSON block
    for (open_ch, close_ch) in [('{', '}'), ('[', ']')] {
        if let Some(start) = text.find(open_ch) {
            let mut depth = 0i32;
            let mut in_string = false;
            let mut escape = false;

            for (i, ch) in text[start..].char_indices() {
                if escape {
                    escape = false;
                    continue;
                }
                if ch == '\\' {
                    escape = true;
                    continue;
                }
                if ch == '"' {
                    in_string = !in_string;
                    continue;
                }
                if in_string {
                    continue;
                }
                if ch == open_ch {
                    depth += 1;
                } else if ch == close_ch {
                    depth -= 1;
                    if depth == 0 {
                        let candidate = &text[start..start + i + 1];
                        if let Ok(v) = serde_json::from_str::<Value>(candidate) {
                            return Some(v);
                        }
                        break;
                    }
                }
            }
        }
    }

    warn!(
        "All JSON extraction strategies failed for step '{}'",
        step_id
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_json_direct() {
        let json_str = r#"{"key": "value"}"#;
        let result = parse_json_output(json_str, "test");
        assert!(result.is_some());
    }

    #[test]
    fn test_parse_json_from_fence() {
        let text = "Here is the result:\n```json\n{\"key\": \"value\"}\n```\nDone.";
        let result = parse_json_output(text, "test");
        assert!(result.is_some());
    }

    #[test]
    fn test_parse_json_from_balanced() {
        let text = "Some text before {\"key\": \"value\"} and after";
        let result = parse_json_output(text, "test");
        assert!(result.is_some());
    }

    #[test]
    fn test_parse_json_array() {
        let text = "Result: [1, 2, 3]";
        let result = parse_json_output(text, "test");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn test_parse_json_no_json() {
        let text = "This is just plain text with no JSON at all.";
        let result = parse_json_output(text, "test");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_json_fence_without_label() {
        let text = "```\n{\"a\": 1}\n```";
        let result = parse_json_output(text, "test");
        assert!(result.is_some());
    }

    #[test]
    fn test_parse_json_nested_braces() {
        let text = r#"Here: {"outer": {"inner": [1, 2]}} done"#;
        let result = parse_json_output(text, "test");
        assert!(result.is_some());
        let v = result.unwrap();
        assert_eq!(v["outer"]["inner"], serde_json::json!([1, 2]));
    }

    #[test]
    fn test_parse_json_escaped_quotes() {
        let text = r#"{"msg": "he said \"hello\""}"#;
        let result = parse_json_output(text, "test");
        assert!(result.is_some());
    }
}
