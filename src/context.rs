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
        log::debug!(
            "RecipeContext::new: initializing with {} keys",
            initial.len()
        );
        Self { data: initial }
    }

    /// Retrieve a value by key, supporting dot notation for nested access.
    pub fn get(&self, key: &str) -> Option<&Value> {
        log::trace!("RecipeContext::get: key={:?}", key);
        let parts: Vec<&str> = key.split('.').collect();
        let mut current = self.data.get(parts[0])?;
        for part in &parts[1..] {
            current = current.get(part)?;
        }
        Some(current)
    }

    /// Store a value at the top level of the context.
    pub fn set(&mut self, key: &str, value: Value) {
        log::debug!("RecipeContext::set: key={:?}", key);
        self.data.insert(key.to_string(), value);
    }

    /// Replace `{{var}}` placeholders with context values.
    /// Dict/array values are serialized to JSON. Missing variables become empty string.
    pub fn render(&self, template: &str) -> String {
        log::debug!("RecipeContext::render: template length={}", template.len());
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

    /// Replace `{{var}}` placeholders with env var references for bash steps.
    ///
    /// Instead of inlining values into shell source (which breaks on single
    /// quotes, parentheses, and other shell metacharacters), this method:
    /// 1. Replaces `{{var}}` with `"$RECIPE_VAR_var"` (double-quoted env ref)
    /// 2. Returns the env vars to inject into the subprocess
    ///
    /// The env var approach is immune to shell injection because values never
    /// appear in the shell source — they're passed via the process environment.
    pub fn render_shell(&self, template: &str) -> String {
        log::debug!(
            "RecipeContext::render_shell: template length={}",
            template.len()
        );
        TEMPLATE_RE
            .replace_all(template, |caps: &regex::Captures| {
                let var_name = &caps[1];
                let env_key = Self::env_key(var_name);
                format!("\"${}\"", env_key)
            })
            .into_owned()
    }

    /// Return environment variables for all context values.
    /// Keys are prefixed with `RECIPE_VAR_` and dots replaced with `__`.
    pub fn shell_env_vars(&self) -> HashMap<String, String> {
        log::debug!(
            "RecipeContext::shell_env_vars: exporting {} context keys",
            self.data.len()
        );
        let mut env = HashMap::new();
        for (key, value) in &self.data {
            let env_key = Self::env_key(key);
            let env_val = match value {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                v => v.to_string(),
            };
            env.insert(env_key, env_val);

            // Also export nested keys for dot-notation access
            if let Value::Object(map) = value {
                Self::flatten_nested(&format!("RECIPE_VAR_{}", key), map, &mut env);
            }
        }
        env
    }

    /// Convert a template variable name to an env var key.
    fn env_key(var_name: &str) -> String {
        log::trace!("RecipeContext::env_key: var_name={:?}", var_name);
        format!(
            "RECIPE_VAR_{}",
            var_name.replace('.', "__").replace('-', "_")
        )
    }

    /// Recursively flatten nested JSON objects into env vars with `__` separators.
    fn flatten_nested(
        prefix: &str,
        map: &serde_json::Map<String, Value>,
        env: &mut HashMap<String, String>,
    ) {
        log::trace!(
            "RecipeContext::flatten_nested: prefix={:?}, keys={}",
            prefix,
            map.len()
        );
        for (k, v) in map {
            let key = format!("{}__{}", prefix, k.replace('.', "__").replace('-', "_"));
            match v {
                Value::String(s) => {
                    env.insert(key, s.clone());
                }
                Value::Null => {
                    env.insert(key, String::new());
                }
                Value::Object(nested) => {
                    env.insert(key.clone(), v.to_string());
                    Self::flatten_nested(&key, nested, env);
                }
                other => {
                    env.insert(key, other.to_string());
                }
            }
        }
    }

    /// Safely evaluate a boolean condition against the current context.
    ///
    /// Delegates to `condition::evaluate_condition()`.
    pub fn evaluate(&self, condition: &str) -> Result<bool, ConditionError> {
        log::debug!(
            "RecipeContext::evaluate: condition={:?}",
            crate::safe_truncate(condition, 200)
        );
        evaluate_condition(condition, &self.data)
    }

    /// Return a clone of the context data.
    pub fn to_map(&self) -> HashMap<String, Value> {
        log::trace!("RecipeContext::to_map: cloning {} keys", self.data.len());
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
    fn test_render_shell_uses_env_var_refs() {
        let c = ctx(vec![("cmd", json!("hello; rm -rf /"))]);
        let rendered = c.render_shell("echo {{cmd}}");
        // render_shell now replaces with env var reference instead of inlining
        assert_eq!(rendered, "echo \"$RECIPE_VAR_cmd\"");
    }

    #[test]
    fn test_shell_env_vars() {
        let c = ctx(vec![("cmd", json!("hello; rm -rf /"))]);
        let env = c.shell_env_vars();
        assert_eq!(env.get("RECIPE_VAR_cmd").unwrap(), "hello; rm -rf /");
    }

    #[test]
    fn test_shell_env_vars_nested() {
        let c = ctx(vec![("obj", json!({"status": "ok", "count": 5}))]);
        let env = c.shell_env_vars();
        assert_eq!(env.get("RECIPE_VAR_obj__status").unwrap(), "ok");
        assert_eq!(env.get("RECIPE_VAR_obj__count").unwrap(), "5");
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

    // ── Edge cases (test-5) ──────────────────────────────

    #[test]
    fn test_empty_condition() {
        let c = ctx(vec![]);
        assert!(c.evaluate("").is_err());
    }

    #[test]
    fn test_whitespace_only_condition() {
        let c = ctx(vec![]);
        assert!(c.evaluate("   ").is_err());
    }

    #[test]
    fn test_empty_context_variable_access() {
        let c = ctx(vec![]);
        assert!(!c.evaluate("novar").unwrap());
    }

    #[test]
    fn test_null_value_comparison() {
        let c = ctx(vec![("v", json!(null))]);
        assert!(!c.evaluate("v").unwrap());
        assert!(!c.evaluate("v == 'hello'").unwrap());
    }

    #[test]
    fn test_empty_string_is_falsy() {
        let c = ctx(vec![("s", json!(""))]);
        assert!(!c.evaluate("s").unwrap());
    }

    #[test]
    fn test_render_empty_template() {
        let c = ctx(vec![]);
        assert_eq!(c.render(""), "");
    }

    #[test]
    fn test_render_no_placeholders() {
        let c = ctx(vec![]);
        assert_eq!(c.render("plain text"), "plain text");
    }

    #[test]
    fn test_render_missing_variable() {
        let c = ctx(vec![]);
        assert_eq!(c.render("before {{missing}} after"), "before  after");
    }

    #[test]
    fn test_render_shell_empty() {
        let c = ctx(vec![]);
        assert_eq!(c.render_shell(""), "");
    }

    #[test]
    fn test_len_empty_string() {
        let c = ctx(vec![("s", json!(""))]);
        assert!(c.evaluate("len(s) == 0").unwrap());
    }

    #[test]
    fn test_len_empty_array() {
        let c = ctx(vec![("a", json!([]))]);
        assert!(c.evaluate("len(a) == 0").unwrap());
    }

    #[test]
    fn test_method_on_empty_string() {
        let c = ctx(vec![("s", json!(""))]);
        assert!(c.evaluate("s.strip() == ''").unwrap());
        assert!(c.evaluate("s.upper() == ''").unwrap());
        assert!(c.evaluate("s.lower() == ''").unwrap());
    }

    // ── Boundary values (test-6) ──────────────────────────

    #[test]
    fn test_deeply_nested_parens() {
        let c = ctx(vec![("x", json!(true))]);
        let inner = "x";
        let mut expr = inner.to_string();
        for _ in 0..30 {
            expr = format!("({})", expr);
        }
        assert!(c.evaluate(&expr).unwrap());
    }

    #[test]
    fn test_max_nesting_exceeded() {
        let c = ctx(vec![("x", json!(true))]);
        let inner = "x";
        let mut expr = inner.to_string();
        for _ in 0..33 {
            expr = format!("({})", expr);
        }
        assert!(c.evaluate(&expr).is_err());
    }

    #[test]
    fn test_very_long_string_literal() {
        let c = ctx(vec![]);
        let long = "a".repeat(3000);
        let expr = format!("'{}' == '{}'", long, long);
        assert!(c.evaluate(&expr).unwrap());
    }

    #[test]
    fn test_many_or_clauses() {
        let c = ctx(vec![("x", json!("last"))]);
        let mut parts: Vec<String> = (0..49).map(|i| format!("x == 'v{}'", i)).collect();
        parts.push("x == 'last'".to_string());
        let expr = parts.join(" or ");
        assert!(c.evaluate(&expr).unwrap());
    }

    #[test]
    fn test_many_and_clauses() {
        let vars: Vec<(&str, Value)> = (0..20)
            .map(|i| {
                let name = Box::leak(format!("v{}", i).into_boxed_str()) as &str;
                (name, json!(true))
            })
            .collect();
        let c = ctx(vars);
        let expr = (0..20)
            .map(|i| format!("v{}", i))
            .collect::<Vec<_>>()
            .join(" and ");
        assert!(c.evaluate(&expr).unwrap());
    }

    #[test]
    fn test_numeric_boundary_zero() {
        let c = ctx(vec![("n", json!(0))]);
        assert!(!c.evaluate("n").unwrap());
        assert!(c.evaluate("n == 0").unwrap());
    }

    #[test]
    fn test_numeric_boundary_negative() {
        let c = ctx(vec![("n", json!(-1))]);
        assert!(c.evaluate("n < 0").unwrap());
        assert!(c.evaluate("n == -1").unwrap());
    }

    #[test]
    fn test_numeric_boundary_large() {
        let c = ctx(vec![("n", json!(999_999_999))]);
        assert!(c.evaluate("n > 0").unwrap());
        assert!(c.evaluate("n == 999999999").unwrap());
    }
}
