/// Recipe execution context with template rendering and safe expression evaluation.
///
/// Provides variable storage, dot-notation access, Mustache-style template rendering,
/// and a safe condition evaluator. Direct port from Python `amplihack.recipes.context`.
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
                    None => String::new(),
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
                    None => String::new(),
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
    /// Supports a subset of comparison expressions:
    /// - `var == "value"`, `var != "value"`
    /// - `"substring" in var`, `"substring" not in var`
    /// - `var and var2`, `var or var2`, `not var`
    /// - Parenthesized sub-expressions
    pub fn evaluate(&self, condition: &str) -> Result<bool, ConditionError> {
        if condition.contains("__") {
            return Err(ConditionError::Unsafe(
                "dunder attribute access is not allowed".to_string(),
            ));
        }
        let tokens = tokenize(condition)?;
        let mut parser = ExprParser::new(&tokens, &self.data);
        let result = parser.parse_or()?;
        Ok(result)
    }

    /// Return a clone of the context data.
    pub fn to_map(&self) -> HashMap<String, Value> {
        self.data.clone()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConditionError {
    #[error("Unsafe expression: {0}")]
    Unsafe(String),
    #[error("Parse error: {0}")]
    Parse(String),
}

// ── Simple tokenizer and recursive-descent parser for conditions ──

#[derive(Debug, Clone, PartialEq)]
enum Token {
    String(String),    // quoted string literal
    Number(f64),       // numeric literal
    Ident(String),     // variable name (may contain dots)
    Eq,                // ==
    NotEq,             // !=
    In,                // in
    NotIn,             // not in
    And,               // and
    Or,                // or
    Not,               // not (standalone, not followed by 'in')
    LParen,            // (
    RParen,            // )
}

fn tokenize(input: &str) -> Result<Vec<Token>, ConditionError> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            ' ' | '\t' | '\n' | '\r' => i += 1,
            '(' => { tokens.push(Token::LParen); i += 1; }
            ')' => { tokens.push(Token::RParen); i += 1; }
            '=' if i + 1 < chars.len() && chars[i + 1] == '=' => {
                tokens.push(Token::Eq); i += 2;
            }
            '!' if i + 1 < chars.len() && chars[i + 1] == '=' => {
                tokens.push(Token::NotEq); i += 2;
            }
            '\'' | '"' => {
                let quote = chars[i];
                i += 1;
                let mut s = String::new();
                while i < chars.len() && chars[i] != quote {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                        s.push(chars[i]);
                    } else {
                        s.push(chars[i]);
                    }
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(ConditionError::Parse("unterminated string".to_string()));
                }
                i += 1; // skip closing quote
                tokens.push(Token::String(s));
            }
            c if c.is_ascii_digit() || c == '-' => {
                let start = i;
                if c == '-' { i += 1; }
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let num_str: String = chars[start..i].iter().collect();
                match num_str.parse::<f64>() {
                    Ok(n) => tokens.push(Token::Number(n)),
                    Err(_) => return Err(ConditionError::Parse(format!("invalid number: {}", num_str))),
                }
            }
            c if c.is_ascii_alphanumeric() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '.') {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                match word.as_str() {
                    "and" => tokens.push(Token::And),
                    "or" => tokens.push(Token::Or),
                    "not" => {
                        // Look ahead for "not in"
                        let mut j = i;
                        while j < chars.len() && chars[j] == ' ' { j += 1; }
                        if j + 2 <= chars.len() {
                            let next: String = chars[j..j+2].iter().collect();
                            if next == "in" && (j + 2 >= chars.len() || !chars[j+2].is_ascii_alphanumeric()) {
                                tokens.push(Token::NotIn);
                                i = j + 2;
                            } else {
                                tokens.push(Token::Not);
                            }
                        } else {
                            tokens.push(Token::Not);
                        }
                    }
                    "in" => tokens.push(Token::In),
                    "true" | "True" => tokens.push(Token::Ident("true".to_string())),
                    "false" | "False" => tokens.push(Token::Ident("false".to_string())),
                    _ => tokens.push(Token::Ident(word)),
                }
            }
            c => return Err(ConditionError::Parse(format!("unexpected character: '{}'", c))),
        }
    }

    Ok(tokens)
}

struct ExprParser<'a> {
    tokens: &'a [Token],
    pos: usize,
    data: &'a HashMap<String, Value>,
}

impl<'a> ExprParser<'a> {
    fn new(tokens: &'a [Token], data: &'a HashMap<String, Value>) -> Self {
        Self { tokens, pos: 0, data }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<&Token> {
        let tok = self.tokens.get(self.pos);
        self.pos += 1;
        tok
    }

    // or_expr: and_expr ('or' and_expr)*
    fn parse_or(&mut self) -> Result<bool, ConditionError> {
        let mut result = self.parse_and()?;
        while self.peek() == Some(&Token::Or) {
            self.advance();
            let rhs = self.parse_and()?;
            result = result || rhs;
        }
        Ok(result)
    }

    // and_expr: not_expr ('and' not_expr)*
    fn parse_and(&mut self) -> Result<bool, ConditionError> {
        let mut result = self.parse_not()?;
        while self.peek() == Some(&Token::And) {
            self.advance();
            let rhs = self.parse_not()?;
            result = result && rhs;
        }
        Ok(result)
    }

    // not_expr: 'not' not_expr | comparison
    fn parse_not(&mut self) -> Result<bool, ConditionError> {
        if self.peek() == Some(&Token::Not) {
            self.advance();
            let val = self.parse_not()?;
            return Ok(!val);
        }
        self.parse_comparison()
    }

    // comparison: primary (('==' | '!=' | 'in' | 'not in') primary)?
    fn parse_comparison(&mut self) -> Result<bool, ConditionError> {
        let lhs = self.parse_primary()?;

        match self.peek() {
            Some(Token::Eq) => {
                self.advance();
                let rhs = self.parse_primary()?;
                Ok(values_equal(&lhs, &rhs))
            }
            Some(Token::NotEq) => {
                self.advance();
                let rhs = self.parse_primary()?;
                Ok(!values_equal(&lhs, &rhs))
            }
            Some(Token::In) => {
                self.advance();
                let rhs = self.parse_primary()?;
                Ok(value_in(&lhs, &rhs))
            }
            Some(Token::NotIn) => {
                self.advance();
                let rhs = self.parse_primary()?;
                Ok(!value_in(&lhs, &rhs))
            }
            _ => Ok(is_truthy(&lhs)),
        }
    }

    // primary: STRING | NUMBER | IDENT | '(' or_expr ')'
    fn parse_primary(&mut self) -> Result<Value, ConditionError> {
        match self.peek().cloned() {
            Some(Token::String(s)) => {
                self.advance();
                Ok(Value::String(s))
            }
            Some(Token::Number(n)) => {
                self.advance();
                Ok(serde_json::Number::from_f64(n)
                    .map(Value::Number)
                    .unwrap_or(Value::Null))
            }
            Some(Token::Ident(name)) => {
                self.advance();
                Ok(self.resolve_ident(&name))
            }
            Some(Token::LParen) => {
                self.advance();
                let result = self.parse_or()?;
                if self.peek() != Some(&Token::RParen) {
                    return Err(ConditionError::Parse("expected ')'".to_string()));
                }
                self.advance();
                Ok(Value::Bool(result))
            }
            Some(tok) => Err(ConditionError::Parse(format!("unexpected token: {:?}", tok))),
            None => Err(ConditionError::Parse("unexpected end of expression".to_string())),
        }
    }

    fn resolve_ident(&self, name: &str) -> Value {
        if name == "true" {
            return Value::Bool(true);
        }
        if name == "false" {
            return Value::Bool(false);
        }

        // Support dot notation
        let parts: Vec<&str> = name.split('.').collect();
        let mut current = match self.data.get(parts[0]) {
            Some(v) => v.clone(),
            None => return Value::Null,
        };
        for part in &parts[1..] {
            current = match current.get(part) {
                Some(v) => v.clone(),
                None => return Value::Null,
            };
        }
        current
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    // Coerce: compare string representations for mixed types
    match (a, b) {
        (Value::String(sa), Value::String(sb)) => sa == sb,
        (Value::Number(na), Value::Number(nb)) => na.as_f64() == nb.as_f64(),
        (Value::Bool(ba), Value::Bool(bb)) => ba == bb,
        (Value::Null, Value::Null) => true,
        _ => a.to_string() == b.to_string(),
    }
}

fn value_in(needle: &Value, haystack: &Value) -> bool {
    match haystack {
        Value::String(s) => {
            if let Value::String(n) = needle {
                s.contains(n.as_str())
            } else {
                s.contains(&needle.to_string())
            }
        }
        Value::Array(arr) => arr.iter().any(|item| values_equal(needle, item)),
        _ => false,
    }
}

fn is_truthy(val: &Value) -> bool {
    match val {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
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
        let c = ctx(vec![
            ("a", json!("yes")),
            ("b", json!("")),
        ]);
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
}
