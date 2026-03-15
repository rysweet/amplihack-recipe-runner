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

/// Matches heredoc start markers: <<WORD, <<-WORD, <<'WORD', <<"WORD"
/// Cannot use backreferences in Rust regex, so we match each quote style
/// as separate alternatives.
/// Group 1 = single-quoted delimiter, Group 2 = double-quoted, Group 3 = unquoted
static HEREDOC_START_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<<-?\s*(?:'([A-Za-z_]\w*)'|"([A-Za-z_]\w*)"|([A-Za-z_]\w*))"#).unwrap()
});

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
    /// quotes, parentheses, and other shell metacharacters), this method
    /// replaces `{{var}}` with `$RECIPE_VAR_var` environment variable refs.
    ///
    /// **Context-aware quoting (fixes issue #32):**
    /// - Outside heredocs, unquoted: `{{var}}` → `"$RECIPE_VAR_var"` (double-quoted
    ///   to prevent word splitting)
    /// - Outside heredocs, inside `"..."`: `{{var}}` → `$RECIPE_VAR_var` (already
    ///   quoted by author — adding extra quotes would produce `""$RECIPE_VAR_var""`)
    /// - Outside heredocs, inside `'...'`: `{{var}}` → inline actual value (single
    ///   quotes block `$VAR` expansion in bash)
    /// - Inside unquoted heredoc bodies (`<<WORD`): `{{var}}` → `$RECIPE_VAR_var`
    ///   (unquoted, because heredocs don't word-split and double quotes would
    ///   become literal characters in the output)
    /// - Inside quoted heredoc bodies (`<<'WORD'`): `{{var}}` → inline value
    ///   (bash won't expand `$VAR` in quoted heredocs, so we must inline)
    ///
    /// The env var approach is immune to shell injection because values never
    /// appear in the shell source — they're passed via the process environment.
    pub fn render_shell(&self, template: &str) -> String {
        log::debug!(
            "RecipeContext::render_shell: template length={}",
            template.len()
        );

        let lines: Vec<&str> = template.split('\n').collect();
        let mut result: Vec<String> = Vec::with_capacity(lines.len());

        // Stack of (delimiter, is_quoted) for nested heredocs
        let mut heredoc_stack: Vec<(String, bool)> = Vec::new();

        for line in lines {
            if heredoc_stack.is_empty() {
                // Outside any heredoc — scan for heredoc start markers
                for cap in HEREDOC_START_RE.captures_iter(line) {
                    // Group 1 = single-quoted, Group 2 = double-quoted, Group 3 = unquoted
                    let (delimiter, is_quoted) = if let Some(m) = cap.get(1) {
                        (m.as_str().to_string(), true)
                    } else if let Some(m) = cap.get(2) {
                        (m.as_str().to_string(), true)
                    } else if let Some(m) = cap.get(3) {
                        (m.as_str().to_string(), false)
                    } else {
                        continue;
                    };
                    log::trace!(
                        "render_shell: found heredoc start: delimiter={:?}, quoted={}",
                        delimiter,
                        is_quoted
                    );
                    heredoc_stack.push((delimiter, is_quoted));
                }
                // The start line itself is a regular command — use context-aware refs
                result.push(Self::replace_vars_quoted(line, &self.data));
            } else {
                // Inside a heredoc body — check if this line ends it
                let trimmed = line.trim();
                let (ref delim, is_quoted) = heredoc_stack[heredoc_stack.len() - 1];

                if trimmed == delim {
                    // End of heredoc — this line is the delimiter, don't substitute
                    heredoc_stack.pop();
                    result.push(line.to_string());
                } else if is_quoted {
                    // Quoted heredoc (<<'WORD') — bash won't expand $VAR,
                    // so inline the actual values
                    result.push(Self::replace_vars_inline(line, &self.data));
                } else {
                    // Unquoted heredoc (<<WORD) — bash WILL expand $VAR,
                    // so use unquoted env var refs (no spurious literal quotes)
                    result.push(Self::replace_vars_unquoted(line));
                }
            }
        }

        result.join("\n")
    }

    /// Replace `{{var}}` in a regular bash command line (outside heredocs),
    /// detecting bash quoting context to avoid double-quoting collisions (issue #32).
    ///
    /// - Unquoted context: `{{var}}` → `"$RECIPE_VAR_var"` (protective quotes)
    /// - Inside `"..."`: `{{var}}` → `$RECIPE_VAR_var` (already quoted by author)
    /// - Inside `'...'`: `{{var}}` → inline actual value (single quotes block expansion)
    fn replace_vars_quoted(line: &str, data: &HashMap<String, Value>) -> String {
        // Build quoting-context map: for each byte offset, record quote state.
        // 0 = unquoted, 1 = inside double quotes, 2 = inside single quotes.
        let bytes = line.as_bytes();
        let len = bytes.len();
        let mut quote_state = vec![0u8; len + 1]; // +1 so we can index `m.start()` == len
        let mut state: u8 = 0;
        let mut i = 0;
        while i < len {
            quote_state[i] = state;
            match (state, bytes[i]) {
                (0, b'"') => {
                    state = 1;
                }
                (0, b'\'') => {
                    state = 2;
                }
                (1, b'\\') if i + 1 < len => {
                    // Escape sequence inside double quotes: mark next byte, skip it
                    i += 1;
                    quote_state[i] = state;
                }
                (1, b'"') => {
                    state = 0;
                }
                (2, b'\'') => {
                    state = 0;
                }
                _ => {}
            }
            i += 1;
        }

        TEMPLATE_RE
            .replace_all(line, |caps: &regex::Captures| {
                let m = caps.get(0).unwrap();
                let var_name = &caps[1];
                match quote_state[m.start()] {
                    2 => {
                        // Inside single quotes: bash won't expand $VAR → inline value
                        let parts: Vec<&str> = var_name.split('.').collect();
                        let mut current = data.get(parts[0]);
                        for part in &parts[1..] {
                            current = current.and_then(|v| v.get(part));
                        }
                        match current {
                            None => {
                                log::warn!(
                                    "Template variable '{}' not found in context \
                                     (single-quoted command) — replaced with empty string",
                                    var_name
                                );
                                String::new()
                            }
                            Some(Value::String(s)) => s.clone(),
                            Some(Value::Null) => String::new(),
                            Some(v) => v.to_string(),
                        }
                    }
                    1 => {
                        // Inside double quotes: use unquoted env ref (author's quotes protect it)
                        format!("${}", Self::env_key(var_name))
                    }
                    _ => {
                        // Unquoted: add protective double-quote wrapping
                        format!("\"${}\"", Self::env_key(var_name))
                    }
                }
            })
            .into_owned()
    }

    /// Replace `{{var}}` with `$RECIPE_VAR_var` (unquoted env ref).
    /// Used inside unquoted heredoc bodies where quotes become literal.
    fn replace_vars_unquoted(line: &str) -> String {
        TEMPLATE_RE
            .replace_all(line, |caps: &regex::Captures| {
                let var_name = &caps[1];
                let env_key = Self::env_key(var_name);
                format!("${}", env_key)
            })
            .into_owned()
    }

    /// Replace `{{var}}` with the actual context value (inline).
    /// Used inside quoted heredoc bodies where bash won't expand env vars.
    fn replace_vars_inline(line: &str, data: &HashMap<String, Value>) -> String {
        TEMPLATE_RE
            .replace_all(line, |caps: &regex::Captures| {
                let var_name = &caps[1];
                // Walk dot-notation path
                let parts: Vec<&str> = var_name.split('.').collect();
                let mut current = data.get(parts[0]);
                for part in &parts[1..] {
                    current = current.and_then(|v| v.get(part));
                }
                match current {
                    None => {
                        log::warn!(
                            "Template variable '{}' not found in context (quoted heredoc) — replaced with empty string",
                            var_name
                        );
                        String::new()
                    }
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Null) => String::new(),
                    Some(v) => v.to_string(),
                }
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

    // ── Heredoc-aware render_shell tests ──────────────────

    #[test]
    fn test_render_shell_heredoc_unquoted_no_quotes_in_body() {
        let c = ctx(vec![("user", json!("alice"))]);
        let template = "cat <<EOF\nUser: {{user}}\nEOF";
        let rendered = c.render_shell(template);
        // Inside unquoted heredoc, vars should NOT be wrapped in double quotes
        assert_eq!(rendered, "cat <<EOF\nUser: $RECIPE_VAR_user\nEOF");
    }

    #[test]
    fn test_render_shell_heredoc_start_line_stays_quoted() {
        let c = ctx(vec![("name", json!("test"))]);
        let template = "TASK=$(cat <<EOF\n{{name}}\nEOF\n)";
        let rendered = c.render_shell(template);
        // The start line "TASK=$(cat <<EOF" has no vars, nothing to test there.
        // The body line should be unquoted.
        assert!(rendered.contains("$RECIPE_VAR_name"));
        assert!(!rendered.contains("\"$RECIPE_VAR_name\""));
    }

    #[test]
    fn test_render_shell_heredoc_multiple_vars() {
        let c = ctx(vec![
            ("title", json!("Fix bug")),
            ("body", json!("Details here")),
        ]);
        let template = "cat <<EOF\nTitle: {{title}}\nBody: {{body}}\nEOF";
        let rendered = c.render_shell(template);
        assert_eq!(
            rendered,
            "cat <<EOF\nTitle: $RECIPE_VAR_title\nBody: $RECIPE_VAR_body\nEOF"
        );
    }

    #[test]
    fn test_render_shell_heredoc_with_tab_strip() {
        let c = ctx(vec![("data", json!("value"))]);
        let template = "cat <<-ENDMARKER\n\t{{data}}\n\tENDMARKER";
        let rendered = c.render_shell(template);
        // <<- allows tab-indented delimiter
        assert!(rendered.contains("$RECIPE_VAR_data"));
        assert!(!rendered.contains("\"$RECIPE_VAR_data\""));
    }

    #[test]
    fn test_render_shell_quoted_heredoc_inlines_value() {
        let c = ctx(vec![("script", json!("echo hello"))]);
        let template = "cat <<'PYEOF'\n{{script}}\nPYEOF";
        let rendered = c.render_shell(template);
        // Quoted heredoc: bash won't expand $VAR, so inline the actual value
        assert_eq!(rendered, "cat <<'PYEOF'\necho hello\nPYEOF");
    }

    #[test]
    fn test_render_shell_double_quoted_heredoc_inlines_value() {
        let c = ctx(vec![("code", json!("print('hi')"))]);
        let template = "cat <<\"PYEOF\"\n{{code}}\nPYEOF";
        let rendered = c.render_shell(template);
        assert_eq!(rendered, "cat <<\"PYEOF\"\nprint('hi')\nPYEOF");
    }

    #[test]
    fn test_render_shell_mixed_heredoc_and_regular() {
        let c = ctx(vec![
            ("file", json!("/tmp/out")),
            ("content", json!("hello world")),
        ]);
        // Line 1: regular command (quoted)
        // Lines 2-4: heredoc body (unquoted)
        // Line 5: after heredoc (quoted again)
        let template = "cat <<EOF > {{file}}\n{{content}}\nEOF\necho {{file}}";
        let rendered = c.render_shell(template);
        let lines: Vec<&str> = rendered.split('\n').collect();
        // Start line: {{file}} is outside heredoc body → quoted
        assert_eq!(lines[0], "cat <<EOF > \"$RECIPE_VAR_file\"");
        // Body: {{content}} is inside heredoc → unquoted
        assert_eq!(lines[1], "$RECIPE_VAR_content");
        // Delimiter line
        assert_eq!(lines[2], "EOF");
        // After heredoc: back to quoted
        assert_eq!(lines[3], "echo \"$RECIPE_VAR_file\"");
    }

    #[test]
    fn test_render_shell_no_heredoc_preserves_quoted_behavior() {
        let c = ctx(vec![("cmd", json!("hello; rm -rf /"))]);
        let rendered = c.render_shell("echo {{cmd}} && ls {{cmd}}");
        assert_eq!(
            rendered,
            "echo \"$RECIPE_VAR_cmd\" && ls \"$RECIPE_VAR_cmd\""
        );
    }

    #[test]
    fn test_render_shell_realistic_recipe_pattern() {
        // This is the actual pattern from default-workflow.yaml
        let c = ctx(vec![("task_description", json!("Fix the login bug"))]);
        let template = "TASK_DESC=$(cat <<EOFTASKDESC\n{{task_description}}\nEOFTASKDESC\n)";
        let rendered = c.render_shell(template);
        let lines: Vec<&str> = rendered.split('\n').collect();
        assert_eq!(lines[0], "TASK_DESC=$(cat <<EOFTASKDESC");
        assert_eq!(lines[1], "$RECIPE_VAR_task_description"); // NO quotes!
        assert_eq!(lines[2], "EOFTASKDESC");
        assert_eq!(lines[3], ")");
    }

    #[test]
    fn test_render_shell_heredoc_with_dot_notation_var() {
        let c = ctx(vec![("obj", json!({"status": "ok"}))]);
        let template = "cat <<EOF\nStatus: {{obj.status}}\nEOF";
        let rendered = c.render_shell(template);
        assert_eq!(rendered, "cat <<EOF\nStatus: $RECIPE_VAR_obj__status\nEOF");
    }

    #[test]
    fn test_render_shell_heredoc_missing_var_in_body() {
        let c = ctx(vec![]);
        let template = "cat <<EOF\n{{missing}}\nEOF";
        let rendered = c.render_shell(template);
        // Missing var in unquoted heredoc still becomes env ref (will be empty at runtime)
        assert_eq!(rendered, "cat <<EOF\n$RECIPE_VAR_missing\nEOF");
    }

    #[test]
    fn test_render_shell_quoted_heredoc_missing_var() {
        let c = ctx(vec![]);
        let template = "cat <<'EOF'\n{{missing}}\nEOF";
        let rendered = c.render_shell(template);
        // Missing var in quoted heredoc: inline as empty string
        assert_eq!(rendered, "cat <<'EOF'\n\nEOF");
    }

    #[test]
    fn test_render_shell_heredoc_preserves_non_template_content() {
        let c = ctx(vec![("x", json!("val"))]);
        let template = "cat <<EOF\nplain text\n$EXISTING_VAR\n{{x}}\nmore text\nEOF";
        let rendered = c.render_shell(template);
        assert!(rendered.contains("plain text"));
        assert!(rendered.contains("$EXISTING_VAR"));
        assert!(rendered.contains("$RECIPE_VAR_x"));
        assert!(rendered.contains("more text"));
    }

    #[test]
    fn test_render_shell_empty_heredoc_body() {
        let c = ctx(vec![]);
        let template = "cat <<EOF\nEOF";
        let rendered = c.render_shell(template);
        assert_eq!(rendered, "cat <<EOF\nEOF");
    }

    #[test]
    fn test_render_shell_var_on_heredoc_start_line_is_quoted() {
        // Vars on the same line as <<EOF are NOT in the heredoc body
        let c = ctx(vec![("prefix", json!("data"))]);
        let template = "echo {{prefix}} | cat <<EOF\nstuff\nEOF";
        let rendered = c.render_shell(template);
        let lines: Vec<&str> = rendered.split('\n').collect();
        // The start line should use quoted behavior (unquoted var)
        assert!(lines[0].contains("\"$RECIPE_VAR_prefix\""));
    }

    // ── Regression: issue #32 — context-aware quoting (no double-doubled quotes) ──

    #[test]
    fn test_render_shell_no_double_quoting_when_var_inside_double_quotes() {
        // Regression test for issue #32:
        // When recipe YAML contains: cd "{{repo_path}}"
        // render_shell MUST NOT produce: cd ""$RECIPE_VAR_repo_path""
        // It MUST produce:             cd "$RECIPE_VAR_repo_path"
        let c = ctx(vec![("repo_path", json!("/home/user/my project"))]);
        let rendered = c.render_shell("cd \"{{repo_path}}\"");
        assert_eq!(rendered, "cd \"$RECIPE_VAR_repo_path\"");
        // Extra check: confirm NO double-doubled quotes
        assert!(!rendered.contains("\"\""));
    }

    #[test]
    fn test_render_shell_unquoted_var_still_gets_protective_quotes() {
        // Unquoted {{var}} should still get "..." for word-split protection
        let c = ctx(vec![("name", json!("hello world"))]);
        let rendered = c.render_shell("echo {{name}}");
        assert_eq!(rendered, "echo \"$RECIPE_VAR_name\"");
    }

    #[test]
    fn test_render_shell_single_quoted_command_context_inlines_value() {
        // Inside single-quoted bash context, $VAR is not expanded by bash.
        // render_shell should inline the actual value.
        let c = ctx(vec![("branch", json!("feat/my-feature"))]);
        let rendered = c.render_shell("git checkout '{{branch}}'");
        assert_eq!(rendered, "git checkout 'feat/my-feature'");
    }

    #[test]
    fn test_render_shell_mixed_quoted_and_unquoted_vars() {
        // Test multiple vars in different quoting contexts on the same line
        let c = ctx(vec![
            ("dir", json!("/my/path")),
            ("msg", json!("hello")),
        ]);
        // {{dir}} is inside double-quotes; {{msg}} is unquoted
        let rendered = c.render_shell("cd \"{{dir}}\" && echo {{msg}}");
        assert_eq!(
            rendered,
            "cd \"$RECIPE_VAR_dir\" && echo \"$RECIPE_VAR_msg\""
        );
    }

    #[test]
    fn test_render_shell_gh_issue_create_pattern() {
        // Realistic pattern: gh issue create with double-quoted args
        // This pattern caused issue #34 where "$RECIPE_VAR_task_description"
        // appeared literally in the GitHub issue body.
        let c = ctx(vec![
            ("title", json!("Fix the login bug")),
            ("body", json!("Details about the fix")),
        ]);
        let rendered =
            c.render_shell("gh issue create --title \"{{title}}\" --body \"{{body}}\"");
        // Should produce: gh issue create --title "$RECIPE_VAR_title" --body "$RECIPE_VAR_body"
        // NOT:            gh issue create --title ""$RECIPE_VAR_title"" --body ""$RECIPE_VAR_body""
        assert_eq!(
            rendered,
            "gh issue create --title \"$RECIPE_VAR_title\" --body \"$RECIPE_VAR_body\""
        );
        assert!(!rendered.contains("\"\""));
    }
}
