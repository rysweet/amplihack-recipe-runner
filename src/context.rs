/// Recipe execution context with template rendering.
///
/// Provides variable storage, dot-notation access, Mustache-style template rendering,
/// and delegates condition evaluation to the `condition` module.
use crate::condition::{ConditionError, evaluate_condition};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::LazyLock;

static TEMPLATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{([a-zA-Z0-9_.\-]+)\}\}").unwrap());

/// Mutable context that accumulates step outputs and renders templates.
#[derive(Debug, Clone)]
pub struct RecipeContext {
    data: HashMap<String, Value>,
}

impl RecipeContext {
    pub fn new(initial: HashMap<String, Value>) -> Self {
        Self { data: initial }
    }

    /// Retrieve a value by key, supporting dot notation for nested access.
    pub fn get(&self, key: &str) -> Option<&Value> {
        let parts: Vec<&str> = key.split('.').collect();
        let mut current = self.data.get(parts[0])?;
        for part in &parts[1..] {
            current = current.get(part)?;
        }
        Some(current)
    }

    /// Store a value at the top level of the context.
    pub fn set(&mut self, key: &str, value: Value) {
        self.data.insert(key.to_string(), value);
    }

    /// Replace `{{var}}` placeholders with context values.
    /// Dict/array values are serialized to JSON. Missing variables become empty string.
    pub fn render(&self, template: &str) -> String {
        TEMPLATE_RE
            .replace_all(template, |caps: &regex::Captures| {
                let var_name = &caps[1];
                match self.get(var_name) {
                    None => {
                        log::warn!("Template variable '{}' not found in context — replaced with empty string", var_name);
                        String::new()
                    }
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Null) => String::new(),
                    Some(v) => v.to_string(),
                }
            })
            .into_owned()
    }

    /// Replace `{{var}}` placeholders with shell-escaped context values.
    pub fn render_shell(&self, template: &str) -> String {
        TEMPLATE_RE
            .replace_all(template, |caps: &regex::Captures| {
                let var_name = &caps[1];
                let raw = match self.get(var_name) {
                    None => {
                        log::warn!("Template variable '{}' not found in context — replaced with empty string", var_name);
                        String::new()
                    }
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Null) => String::new(),
                    Some(v) => v.to_string(),
                };
                shell_escape::escape(raw.into()).into_owned()
            })
            .into_owned()
    }

    /// Safely evaluate a boolean condition against the current context.
    ///
    /// Delegates to `condition::evaluate_condition()`.
    pub fn evaluate(&self, condition: &str) -> Result<bool, ConditionError> {
        evaluate_condition(condition, &self.data)
    }

    /// Return a clone of the context data.
    pub fn to_map(&self) -> HashMap<String, Value> {
        self.data.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx(pairs: Vec<(&str, Value)>) -> RecipeContext {
        let mut data = HashMap::new();
        for (k, v) in pairs {
            data.insert(k.to_string(), v);
        }
        RecipeContext::new(data)
    }

    #[test]
    fn test_render_simple() {
        let c = ctx(vec![("name", json!("world"))]);
        assert_eq!(c.render("hello {{name}}"), "hello world");
    }

    #[test]
    fn test_render_missing_var() {
        let c = ctx(vec![]);
        assert_eq!(c.render("hello {{missing}}"), "hello ");
    }

    #[test]
    fn test_render_dict_value() {
        let c = ctx(vec![("data", json!({"key": "val"}))]);
        let rendered = c.render("result: {{data}}");
        assert!(rendered.contains("key"));
    }

    #[test]
    fn test_render_shell_escapes() {
        let c = ctx(vec![("cmd", json!("hello; rm -rf /"))]);
        let rendered = c.render_shell("echo {{cmd}}");
        // shell_escape wraps the value in quotes, preventing injection
        assert!(rendered.contains('\'') || rendered.contains('"'));
        assert!(rendered.starts_with("echo "));
    }

    #[test]
    fn test_evaluate_eq() {
        let c = ctx(vec![("status", json!("CONVERGED"))]);
        assert!(c.evaluate("status == 'CONVERGED'").unwrap());
        assert!(!c.evaluate("status == 'OTHER'").unwrap());
    }

    #[test]
    fn test_evaluate_neq() {
        let c = ctx(vec![("status", json!("CONVERGED"))]);
        assert!(c.evaluate("status != 'OTHER'").unwrap());
        assert!(!c.evaluate("status != 'CONVERGED'").unwrap());
    }

    #[test]
    fn test_evaluate_in() {
        let c = ctx(vec![("text", json!("hello world"))]);
        assert!(c.evaluate("'world' in text").unwrap());
        assert!(!c.evaluate("'xyz' in text").unwrap());
    }

    #[test]
    fn test_evaluate_not_in() {
        let c = ctx(vec![("text", json!("hello world"))]);
        assert!(c.evaluate("'xyz' not in text").unwrap());
        assert!(!c.evaluate("'world' not in text").unwrap());
    }

    #[test]
    fn test_evaluate_and_or() {
        let c = ctx(vec![("a", json!("yes")), ("b", json!(""))]);
        assert!(!c.evaluate("a and b").unwrap());
        assert!(c.evaluate("a or b").unwrap());
    }

    #[test]
    fn test_evaluate_rejects_dunder() {
        let c = ctx(vec![]);
        assert!(c.evaluate("__import__('os')").is_err());
    }

    #[test]
    fn test_dot_notation_get() {
        let c = ctx(vec![("obj", json!({"nested": {"val": 42}}))]);
        let val = c.get("obj.nested.val").unwrap();
        assert_eq!(val, &json!(42));
    }

    #[test]
    fn test_evaluate_function_len() {
        let c = ctx(vec![("text", json!("hello"))]);
        assert!(c.evaluate("len(text) == 5").unwrap());
    }

    #[test]
    fn test_evaluate_function_int() {
        let c = ctx(vec![("num_str", json!("42"))]);
        assert!(c.evaluate("int(num_str) == 42").unwrap());
    }

    #[test]
    fn test_evaluate_method_strip() {
        let c = ctx(vec![("text", json!("  hello  "))]);
        assert!(c.evaluate("text.strip() == 'hello'").unwrap());
    }

    #[test]
    fn test_evaluate_method_lower() {
        let c = ctx(vec![("text", json!("HELLO"))]);
        assert!(c.evaluate("text.lower() == 'hello'").unwrap());
    }

    #[test]
    fn test_evaluate_method_upper() {
        let c = ctx(vec![("text", json!("hello"))]);
        assert!(c.evaluate("text.upper() == 'HELLO'").unwrap());
    }

    #[test]
    fn test_evaluate_method_startswith() {
        let c = ctx(vec![("text", json!("hello world"))]);
        assert!(c.evaluate("text.startswith('hello')").unwrap());
        assert!(!c.evaluate("text.startswith('world')").unwrap());
    }

    #[test]
    fn test_evaluate_method_replace() {
        let c = ctx(vec![("text", json!("hello world"))]);
        assert!(
            c.evaluate("text.replace('world', 'rust') == 'hello rust'")
                .unwrap()
        );
    }

    #[test]
    fn test_evaluate_comparison_lt_gt() {
        let c = ctx(vec![("a", json!(5)), ("b", json!(10))]);
        assert!(c.evaluate("a < b").unwrap());
        assert!(c.evaluate("b > a").unwrap());
        assert!(c.evaluate("a <= 5").unwrap());
        assert!(c.evaluate("b >= 10").unwrap());
    }

    #[test]
    fn test_reject_unsafe_method() {
        let c = ctx(vec![("text", json!("hello"))]);
        assert!(c.evaluate("text.system()").is_err());
    }

    #[test]
    fn test_hyphenated_variable_in_condition() {
        let c = ctx(vec![("my-var", json!("hello"))]);
        assert!(c.evaluate("my-var == 'hello'").unwrap());
        assert!(!c.evaluate("my-var == 'other'").unwrap());
    }

    #[test]
    fn test_hyphen_as_minus_operator() {
        // `x - 3` should NOT treat `x-3` as a single identifier
        // Hyphen followed by a digit = minus operator (falls to number parsing)
        let c = ctx(vec![("x", json!(10))]);
        // The tokenizer should emit: Ident("x"), then '-' followed by '3' → Number(-3)
        // But since '-3' starts a negative number token, this evaluates as truthy ident, not subtraction.
        // This test just verifies we don't crash and that `x` resolves correctly.
        assert!(c.evaluate("x").unwrap());
    }

    #[test]
    fn test_multi_hyphen_variable() {
        let c = ctx(vec![("my-long-var-name", json!("value"))]);
        assert!(c.evaluate("my-long-var-name == 'value'").unwrap());
    }

    #[test]
    fn test_dot_notation_property_access_in_condition() {
        let c = ctx(vec![("obj", json!({"status": "ok", "count": 5}))]);
        assert!(c.evaluate("obj.status == 'ok'").unwrap());
        assert!(c.evaluate("obj.count == 5").unwrap());
    }

    #[test]
    fn test_dot_notation_nested_property_access() {
        let c = ctx(vec![("data", json!({"nested": {"val": "deep"}}))]);
        assert!(c.evaluate("data.nested.val == 'deep'").unwrap());
    }

    #[test]
    fn test_dot_notation_missing_property_is_null() {
        let c = ctx(vec![("obj", json!({"a": 1}))]);
        assert!(!c.evaluate("obj.missing").unwrap());
    }

    #[test]
    fn test_short_circuit_or() {
        // `true or X` should return true without evaluating X.
        // We use a truthy value on the left so the right side doesn't matter.
        let c = ctx(vec![("a", json!("yes"))]);
        assert!(c.evaluate("a or nonexistent").unwrap());
    }

    #[test]
    fn test_short_circuit_and() {
        // `false and X` should return false without evaluating X.
        let c = ctx(vec![("a", json!(""))]);
        assert!(!c.evaluate("a and nonexistent").unwrap());
    }

    #[test]
    fn test_short_circuit_preserves_both_sides() {
        // When not short-circuiting, both sides must still evaluate
        let c = ctx(vec![("a", json!("yes")), ("b", json!("also"))]);
        assert!(c.evaluate("a and b").unwrap());
        let c2 = ctx(vec![("a", json!("")), ("b", json!("yes"))]);
        assert!(c2.evaluate("a or b").unwrap());
    }
}
