use log::debug;
/// Safe condition evaluator for recipe step conditions.
///
/// Provides a tokenizer, recursive-descent parser, and built-in function/method
/// library for evaluating boolean expressions over recipe context variables.
use serde_json::Value;
use std::collections::HashMap;

/// Maximum length of a condition expression (bytes).
pub(crate) const MAX_CONDITION_LEN: usize = 8192;

/// Evaluate a condition expression against a context data map.
///
/// Returns `Ok(true)` or `Ok(false)`, or a `ConditionError` for invalid/unsafe expressions.
pub(crate) fn evaluate_condition(
    condition: &str,
    data: &HashMap<String, Value>,
) -> Result<bool, ConditionError> {
    debug!(
        "evaluate_condition: condition={:?}",
        crate::safe_truncate(condition, 200)
    );
    if condition.len() > MAX_CONDITION_LEN {
        return Err(ConditionError::Parse(format!(
            "condition expression too long ({} bytes, max {})",
            condition.len(),
            MAX_CONDITION_LEN
        )));
    }
    let tokens = tokenize(condition)?;
    let mut parser = ExprParser::new(&tokens, data);
    parser.parse_or()
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
    String(String), // quoted string literal
    Number(f64),    // numeric literal
    Ident(String),  // variable name (may contain dots)
    Eq,             // ==
    NotEq,          // !=
    Lt,             // <
    LtEq,           // <=
    Gt,             // >
    GtEq,           // >=
    In,             // in
    NotIn,          // not in
    And,            // and
    Or,             // or
    Not,            // not (standalone, not followed by 'in')
    LParen,         // (
    RParen,         // )
    LBracket,       // [
    RBracket,       // ]
    Comma,          // ,
    Dot,            // .  (for method calls)
}

fn tokenize(input: &str) -> Result<Vec<Token>, ConditionError> {
    debug!("tokenize: input length={}", input.len());
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            ' ' | '\t' | '\n' | '\r' => i += 1,
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            '[' => {
                tokens.push(Token::LBracket);
                i += 1;
            }
            ']' => {
                tokens.push(Token::RBracket);
                i += 1;
            }
            ',' => {
                tokens.push(Token::Comma);
                i += 1;
            }
            '=' if i + 1 < chars.len() && chars[i + 1] == '=' => {
                tokens.push(Token::Eq);
                i += 2;
            }
            '!' if i + 1 < chars.len() && chars[i + 1] == '=' => {
                tokens.push(Token::NotEq);
                i += 2;
            }
            '<' if i + 1 < chars.len() && chars[i + 1] == '=' => {
                tokens.push(Token::LtEq);
                i += 2;
            }
            '<' => {
                tokens.push(Token::Lt);
                i += 1;
            }
            '>' if i + 1 < chars.len() && chars[i + 1] == '=' => {
                tokens.push(Token::GtEq);
                i += 2;
            }
            '>' => {
                tokens.push(Token::Gt);
                i += 1;
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
            c if c.is_ascii_digit()
                || (c == '-' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit()) =>
            {
                let start = i;
                if c == '-' {
                    i += 1;
                }
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let num_str: String = chars[start..i].iter().collect();
                match num_str.parse::<f64>() {
                    Ok(n) => tokens.push(Token::Number(n)),
                    Err(_) => {
                        return Err(ConditionError::Parse(format!(
                            "invalid number: {}",
                            num_str
                        )));
                    }
                }
            }
            c if c.is_ascii_alphanumeric() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                // Extend identifier to include hyphens when followed by a letter
                // (e.g. `my-var`), but NOT when followed by a digit or space
                // (which would be a minus operator like `x - 3`).
                while i < chars.len()
                    && chars[i] == '-'
                    && i + 1 < chars.len()
                    && chars[i + 1].is_ascii_alphabetic()
                {
                    i += 1; // consume hyphen
                    while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                        i += 1;
                    }
                }
                let word: String = chars[start..i].iter().collect();
                match word.as_str() {
                    "and" => tokens.push(Token::And),
                    "or" => tokens.push(Token::Or),
                    "not" => {
                        // Look ahead for "not in"
                        let mut j = i;
                        while j < chars.len() && chars[j] == ' ' {
                            j += 1;
                        }
                        if j + 2 <= chars.len() {
                            let next: String = chars[j..j + 2].iter().collect();
                            if next == "in"
                                && (j + 2 >= chars.len() || !chars[j + 2].is_ascii_alphanumeric())
                            {
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
            '.' => {
                // Dot for method calls (e.g., value.strip())
                tokens.push(Token::Dot);
                i += 1;
            }
            c => {
                return Err(ConditionError::Parse(format!(
                    "unexpected character: '{}'",
                    c
                )));
            }
        }
    }

    Ok(tokens)
}

struct ExprParser<'a> {
    tokens: &'a [Token],
    pos: usize,
    data: &'a HashMap<String, Value>,
    depth: usize,
}

impl<'a> ExprParser<'a> {
    fn new(tokens: &'a [Token], data: &'a HashMap<String, Value>) -> Self {
        Self {
            tokens,
            pos: 0,
            data,
            depth: 0,
        }
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
    // Short-circuits: if left is truthy, skip evaluating right.
    fn parse_or(&mut self) -> Result<bool, ConditionError> {
        log::trace!("ExprParser::parse_or: pos={}", self.pos);
        let mut result = self.parse_and()?;
        while self.peek() == Some(&Token::Or) {
            self.advance();
            if result {
                // Short-circuit: left is truthy, skip right but still parse it
                // to advance the token position.
                let _rhs = self.parse_and()?;
            } else {
                result = self.parse_and()?;
            }
        }
        Ok(result)
    }

    // and_expr: not_expr ('and' not_expr)*
    // Short-circuits: if left is falsy, skip evaluating right.
    fn parse_and(&mut self) -> Result<bool, ConditionError> {
        log::trace!("ExprParser::parse_and: pos={}", self.pos);
        let mut result = self.parse_not()?;
        while self.peek() == Some(&Token::And) {
            self.advance();
            if !result {
                // Short-circuit: left is falsy, skip right but still parse it
                // to advance the token position.
                let _rhs = self.parse_not()?;
            } else {
                result = self.parse_not()?;
            }
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

    // comparison: primary (('==' | '!=' | '<' | '<=' | '>' | '>=' | 'in' | 'not in') primary)?
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
            Some(Token::Lt) => {
                self.advance();
                let rhs = self.parse_primary()?;
                Ok(compare_values(&lhs, &rhs) == Some(std::cmp::Ordering::Less))
            }
            Some(Token::LtEq) => {
                self.advance();
                let rhs = self.parse_primary()?;
                Ok(matches!(
                    compare_values(&lhs, &rhs),
                    Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                ))
            }
            Some(Token::Gt) => {
                self.advance();
                let rhs = self.parse_primary()?;
                Ok(compare_values(&lhs, &rhs) == Some(std::cmp::Ordering::Greater))
            }
            Some(Token::GtEq) => {
                self.advance();
                let rhs = self.parse_primary()?;
                Ok(matches!(
                    compare_values(&lhs, &rhs),
                    Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                ))
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

    // primary: atom postfix*
    // postfix: '.' IDENT '(' args ')' (method call)
    //        | '.' IDENT              (property access — dot-notation context lookup)
    //        | '[' STRING ']'         (object key access)
    //        | '[' NUMBER ']'         (array index access)
    fn parse_primary(&mut self) -> Result<Value, ConditionError> {
        let mut value = self.parse_atom()?;

        // Handle postfix access and method calls.
        loop {
            match self.peek() {
                Some(Token::Dot) => {
                    self.advance(); // consume '.'
                    let method_name = match self.peek().cloned() {
                        Some(Token::Ident(name)) => {
                            self.advance();
                            name
                        }
                        _ => {
                            return Err(ConditionError::Parse(
                                "expected method name after '.'".to_string(),
                            ));
                        }
                    };

                    value = self.parse_dot_access(value, &method_name)?;
                }
                Some(Token::LBracket) => {
                    value = self.parse_bracket_access(value)?;
                }
                _ => break,
            }
        }

        Ok(value)
    }

    /// Handle dot-access: safe method call, unsafe method rejection, or property access.
    fn parse_dot_access(
        &mut self,
        value: Value,
        method_name: &str,
    ) -> Result<Value, ConditionError> {
        if self.peek() == Some(&Token::LParen) && SAFE_METHOD_NAMES.contains(&method_name) {
            self.parse_method_call(value, method_name)
        } else if self.peek() == Some(&Token::LParen) {
            Err(ConditionError::Unsafe(format!(
                "method '.{}()' is not allowed. Safe methods: {:?}",
                method_name, SAFE_METHOD_NAMES
            )))
        } else {
            // Dot-notation property access
            if method_name.contains("__") {
                return Err(ConditionError::Unsafe(format!(
                    "dunder property '{}' is not allowed",
                    method_name
                )));
            }
            Ok(match value.get(method_name) {
                Some(v) => v.clone(),
                None => Value::Null,
            })
        }
    }

    /// Parse a safe method call: value.method(args...)
    fn parse_method_call(
        &mut self,
        value: Value,
        method_name: &str,
    ) -> Result<Value, ConditionError> {
        self.advance(); // consume '('

        let mut args = Vec::new();
        if self.peek() != Some(&Token::RParen) {
            args.push(self.parse_or_value()?);
            while self.peek() == Some(&Token::Comma) {
                self.advance();
                args.push(self.parse_or_value()?);
            }
        }

        if self.peek() != Some(&Token::RParen) {
            return Err(ConditionError::Parse("expected ')'".to_string()));
        }
        self.advance();

        apply_method(&value, method_name, &args)
    }

    /// Parse bracket access: value['key'] or value[0]
    fn parse_bracket_access(&mut self, value: Value) -> Result<Value, ConditionError> {
        self.advance(); // consume '['

        let accessed = match self.peek().cloned() {
            Some(Token::String(key)) => {
                self.advance();
                if key.contains("__") {
                    return Err(ConditionError::Unsafe(format!(
                        "dunder key '{}' is not allowed",
                        key
                    )));
                }
                match value {
                    Value::Object(map) => map.get(&key).cloned().unwrap_or(Value::Null),
                    _ => Value::Null,
                }
            }
            Some(Token::Number(n)) => {
                self.advance();
                if !n.is_finite() || n < 0.0 || n.fract() != 0.0 {
                    return Err(ConditionError::Parse(format!(
                        "array index must be a non-negative integer, got {}",
                        n
                    )));
                }
                let index = n as usize;
                match value {
                    Value::Array(items) => items.get(index).cloned().unwrap_or(Value::Null),
                    _ => Value::Null,
                }
            }
            Some(other) => {
                return Err(ConditionError::Parse(format!(
                    "expected string key or numeric index inside brackets, got {:?}",
                    other
                )));
            }
            None => {
                return Err(ConditionError::Parse(
                    "unexpected end of expression inside brackets".to_string(),
                ));
            }
        };

        if self.peek() != Some(&Token::RBracket) {
            return Err(ConditionError::Parse("expected ']'".to_string()));
        }
        self.advance();

        Ok(accessed)
    }

    /// Parse an expression that returns a Value (for function/method args)
    fn parse_or_value(&mut self) -> Result<Value, ConditionError> {
        self.parse_atom()
    }

    // atom: STRING | NUMBER | IDENT | function_call | '(' or_expr ')'
    fn parse_atom(&mut self) -> Result<Value, ConditionError> {
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
                // Block dunder access (e.g. __import__, __class__)
                if name.contains("__") {
                    return Err(ConditionError::Unsafe(format!(
                        "dunder identifier '{}' is not allowed",
                        name
                    )));
                }
                // Check if this is a function call: ident '(' args ')'
                if SAFE_CALL_NAMES.contains(&name.as_str()) && self.peek() == Some(&Token::LParen) {
                    self.advance(); // consume '('
                    let mut args = Vec::new();
                    if self.peek() != Some(&Token::RParen) {
                        args.push(self.parse_primary()?);
                        while self.peek() == Some(&Token::Comma) {
                            self.advance();
                            args.push(self.parse_primary()?);
                        }
                    }
                    if self.peek() != Some(&Token::RParen) {
                        return Err(ConditionError::Parse("expected ')'".to_string()));
                    }
                    self.advance();
                    return apply_function(&name, &args);
                }
                Ok(self.resolve_ident(&name))
            }
            Some(Token::LParen) => {
                self.advance();
                self.depth += 1;
                if self.depth > 32 {
                    return Err(ConditionError::Parse(
                        "condition expression too deeply nested (max 32 levels)".to_string(),
                    ));
                }
                let result = self.parse_or()?;
                self.depth -= 1;
                if self.peek() != Some(&Token::RParen) {
                    return Err(ConditionError::Parse("expected ')'".to_string()));
                }
                self.advance();
                Ok(Value::Bool(result))
            }
            Some(tok) => Err(ConditionError::Parse(format!(
                "unexpected token: {:?}",
                tok
            ))),
            None => Err(ConditionError::Parse(
                "unexpected end of expression".to_string(),
            )),
        }
    }

    fn resolve_ident(&self, name: &str) -> Value {
        if name == "true" {
            return Value::Bool(true);
        }
        if name == "false" {
            return Value::Bool(false);
        }

        // Block dunder access (e.g. __import__, __class__)
        if name.contains("__") {
            return Value::Null;
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

/// Safe function names (pure type-coercion and helpers).
const SAFE_CALL_NAMES: &[&str] = &["int", "str", "len", "bool", "float", "min", "max"];

/// Safe method names (side-effect-free string methods).
const SAFE_METHOD_NAMES: &[&str] = &[
    "strip",
    "lstrip",
    "rstrip",
    "lower",
    "upper",
    "title",
    "startswith",
    "endswith",
    "replace",
    "split",
    "join",
    "count",
    "find",
];

/// Apply a safe built-in function.
fn apply_function(name: &str, args: &[Value]) -> Result<Value, ConditionError> {
    debug!("apply_function: name={:?}, args_count={}", name, args.len());
    match name {
        "int" => {
            let arg = args.first().unwrap_or(&Value::Null);
            let n = match arg {
                Value::Number(n) => n.as_i64().unwrap_or(0),
                Value::String(s) => s.trim().parse::<i64>().map_err(|_| {
                    ConditionError::Parse(format!(
                        "cannot convert '{}' to int",
                        crate::safe_truncate(s, 50)
                    ))
                })?,
                Value::Bool(b) => i64::from(*b),
                _ => 0,
            };
            Ok(Value::Number(serde_json::Number::from(n)))
        }
        "float" => {
            let arg = args.first().unwrap_or(&Value::Null);
            let n = match arg {
                Value::Number(n) => n.as_f64().unwrap_or(0.0),
                Value::String(s) => s.trim().parse::<f64>().map_err(|_| {
                    ConditionError::Parse(format!(
                        "cannot convert '{}' to float",
                        crate::safe_truncate(s, 50)
                    ))
                })?,
                Value::Bool(b) => f64::from(u8::from(*b)),
                _ => 0.0,
            };
            Ok(serde_json::Number::from_f64(n)
                .map(Value::Number)
                .unwrap_or(Value::Null))
        }
        "str" => {
            let arg = args.first().unwrap_or(&Value::Null);
            Ok(Value::String(match arg {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                v => v.to_string(),
            }))
        }
        "bool" => {
            let arg = args.first().unwrap_or(&Value::Null);
            Ok(Value::Bool(is_truthy(arg)))
        }
        "len" => {
            let arg = args.first().unwrap_or(&Value::Null);
            let len = match arg {
                Value::String(s) => s.len() as i64,
                Value::Array(a) => a.len() as i64,
                Value::Object(o) => o.len() as i64,
                _ => 0,
            };
            Ok(Value::Number(serde_json::Number::from(len)))
        }
        "min" => {
            if args.len() < 2 {
                return Err(ConditionError::Parse(
                    "min() requires at least 2 arguments".to_string(),
                ));
            }
            let mut best = &args[0];
            for arg in &args[1..] {
                if compare_values(arg, best) == Some(std::cmp::Ordering::Less) {
                    best = arg;
                }
            }
            Ok(best.clone())
        }
        "max" => {
            if args.len() < 2 {
                return Err(ConditionError::Parse(
                    "max() requires at least 2 arguments".to_string(),
                ));
            }
            let mut best = &args[0];
            for arg in &args[1..] {
                if compare_values(arg, best) == Some(std::cmp::Ordering::Greater) {
                    best = arg;
                }
            }
            Ok(best.clone())
        }
        _ => Err(ConditionError::Unsafe(format!(
            "function '{}' is not allowed",
            name
        ))),
    }
}

/// Apply a safe method call on a value.
///
/// Non-string values (numbers, booleans, null) are coerced to their string
/// representation before applying the method.  Bash step output is stored via
/// `serde_json::from_str` which parses bare `1` as `Value::Number`, so
/// `workstream_count.strip()` would fail without this coercion (fix #3589).
fn apply_method(value: &Value, method: &str, args: &[Value]) -> Result<Value, ConditionError> {
    debug!(
        "apply_method: method={:?}, args_count={}",
        method,
        args.len()
    );
    let coerced: String;
    let s = match value {
        Value::String(s) => s.as_str(),
        Value::Number(n) => {
            coerced = n.to_string();
            coerced.as_str()
        }
        Value::Bool(b) => {
            coerced = b.to_string();
            coerced.as_str()
        }
        Value::Null => "",
        _ => {
            return Err(ConditionError::Parse(format!(
                "method '.{}()' can only be called on strings, numbers, booleans, or null; got {:?}",
                method, value
            )));
        }
    };

    match method {
        "strip" => Ok(Value::String(s.trim().to_string())),
        "lstrip" => Ok(Value::String(s.trim_start().to_string())),
        "rstrip" => Ok(Value::String(s.trim_end().to_string())),
        "lower" => Ok(Value::String(s.to_lowercase())),
        "upper" => Ok(Value::String(s.to_uppercase())),
        "title" => {
            let titled = s
                .split_whitespace()
                .map(|word| {
                    let mut chars = word.chars();
                    match chars.next() {
                        None => String::new(),
                        Some(c) => c.to_uppercase().to_string() + &chars.as_str().to_lowercase(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            Ok(Value::String(titled))
        }
        "startswith" => {
            let prefix = args.first().and_then(|a| a.as_str()).unwrap_or("");
            Ok(Value::Bool(s.starts_with(prefix)))
        }
        "endswith" => {
            let suffix = args.first().and_then(|a| a.as_str()).unwrap_or("");
            Ok(Value::Bool(s.ends_with(suffix)))
        }
        "replace" => {
            let old = args.first().and_then(|a| a.as_str()).unwrap_or("");
            let new = args.get(1).and_then(|a| a.as_str()).unwrap_or("");
            Ok(Value::String(s.replace(old, new)))
        }
        "split" => {
            let sep = args.first().and_then(|a| a.as_str());
            let parts: Vec<Value> = if let Some(sep) = sep {
                s.split(sep).map(|p| Value::String(p.to_string())).collect()
            } else {
                s.split_whitespace()
                    .map(|p| Value::String(p.to_string()))
                    .collect()
            };
            Ok(Value::Array(parts))
        }
        "join" => {
            // join is called on separator with iterable arg
            let arr = args.first().and_then(|a| a.as_array());
            if let Some(arr) = arr {
                let joined: String = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
                    .join(s);
                Ok(Value::String(joined))
            } else {
                Ok(Value::String(String::new()))
            }
        }
        "count" => {
            let sub = args.first().and_then(|a| a.as_str()).unwrap_or("");
            Ok(Value::Number(serde_json::Number::from(
                s.matches(sub).count() as i64,
            )))
        }
        "find" => {
            let sub = args.first().and_then(|a| a.as_str()).unwrap_or("");
            let idx = s.find(sub).map(|i| i as i64).unwrap_or(-1);
            Ok(Value::Number(serde_json::Number::from(idx)))
        }
        _ => Err(ConditionError::Unsafe(format!(
            "method '.{}()' is not allowed",
            method
        ))),
    }
}

fn compare_values(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    log::trace!("compare_values: comparing values");
    match (a, b) {
        (Value::Number(na), Value::Number(nb)) => na.as_f64()?.partial_cmp(&nb.as_f64()?),
        (Value::String(sa), Value::String(sb)) => Some(sa.cmp(sb)),
        (Value::Bool(ba), Value::Bool(bb)) => Some(ba.cmp(bb)),
        // Cross-type: try numeric coercion
        (Value::String(s), Value::Number(n)) => {
            s.trim().parse::<f64>().ok()?.partial_cmp(&n.as_f64()?)
        }
        (Value::Number(n), Value::String(s)) => {
            n.as_f64()?.partial_cmp(&s.trim().parse::<f64>().ok()?)
        }
        // Cross-type Bool/String: coerce bool to string then compare.
        // Matches the coercion in values_equal (see issue #3069).
        // false < true, "false" < "true" (lexicographic), so this is consistent.
        (Value::Bool(b_val), Value::String(s)) => {
            let b_str = if *b_val { "true" } else { "false" };
            Some(b_str.cmp(s.as_str()))
        }
        (Value::String(s), Value::Bool(b_val)) => {
            let b_str = if *b_val { "true" } else { "false" };
            Some(s.as_str().cmp(b_str))
        }
        // Cross-type Bool/Number: false=0, true=1
        (Value::Bool(b_val), Value::Number(n)) => {
            let b_num = if *b_val { 1.0 } else { 0.0 };
            b_num.partial_cmp(&n.as_f64()?)
        }
        (Value::Number(n), Value::Bool(b_val)) => {
            let b_num = if *b_val { 1.0 } else { 0.0 };
            n.as_f64()?.partial_cmp(&b_num)
        }
        _ => None,
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    log::trace!("values_equal: checking equality");
    // Coerce: compare string representations for mixed types
    match (a, b) {
        (Value::String(sa), Value::String(sb)) => sa == sb,
        (Value::Number(na), Value::Number(nb)) => na.as_f64() == nb.as_f64(),
        (Value::Bool(ba), Value::Bool(bb)) => ba == bb,
        (Value::Null, Value::Null) => true,
        // Cross-type number/string: coerce via compare_values so that
        // Number(1) == String("1") is true (bash outputs are often numeric
        // strings stored as JSON numbers after trim + parse).
        (Value::String(_), Value::Number(_)) | (Value::Number(_), Value::String(_)) => {
            compare_values(a, b) == Some(std::cmp::Ordering::Equal)
        }
        // Cross-type bool/string: coerce so that Bool(true) == String("true")
        // and Bool(false) == String("false"). CLI --set parses "true"/"false"
        // as Value::Bool, but recipe YAML contexts and conditions use quoted
        // string literals like 'true'. Without this coercion, conditions like
        // `force_single_workstream == 'true'` fail when the context variable
        // was set via --set (which stores it as Bool). See issue #3069.
        (Value::Bool(b_val), Value::String(s)) | (Value::String(s), Value::Bool(b_val)) => {
            match s.as_str() {
                "true" | "True" => *b_val,
                "false" | "False" => !*b_val,
                _ => false,
            }
        }
        _ => *a == *b,
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
    log::trace!("is_truthy: checking truthiness of {:?}", val);
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

    fn ctx(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    // -- Bool/String cross-type equality (issue #3069) --

    #[test]
    fn test_bool_true_equals_string_true() {
        assert!(values_equal(&json!(true), &json!("true")));
        assert!(values_equal(&json!("true"), &json!(true)));
    }

    #[test]
    fn test_bool_false_equals_string_false() {
        assert!(values_equal(&json!(false), &json!("false")));
        assert!(values_equal(&json!("false"), &json!(false)));
    }

    #[test]
    fn test_bool_true_not_equals_string_false() {
        assert!(!values_equal(&json!(true), &json!("false")));
        assert!(!values_equal(&json!("false"), &json!(true)));
    }

    #[test]
    fn test_bool_false_not_equals_string_true() {
        assert!(!values_equal(&json!(false), &json!("true")));
        assert!(!values_equal(&json!("true"), &json!(false)));
    }

    #[test]
    fn test_bool_not_equals_arbitrary_string() {
        assert!(!values_equal(&json!(true), &json!("yes")));
        assert!(!values_equal(&json!(false), &json!("no")));
    }

    #[test]
    fn test_bool_true_equals_string_true_capitalized() {
        assert!(values_equal(&json!(true), &json!("True")));
        assert!(values_equal(&json!("True"), &json!(true)));
    }

    // -- Condition evaluation with Bool/String coercion (issue #3069) --

    #[test]
    fn test_force_single_workstream_condition() {
        // Reproduces issue #3069: force_single_workstream set via --set as Bool(true)
        // but condition compares against string literal 'true'.
        let data = ctx(&[
            ("task_type", json!("Development")),
            ("workstream_count", json!(2)),
            ("force_single_workstream", json!(true)), // CLI sets this as Bool
            ("recursion_guard", json!("")),
        ]);

        // The execute-single-round-1 condition
        let cond = "('Development' in task_type or 'Investigation' in task_type) and ((workstream_count == '1' or workstream_count == '') or force_single_workstream == 'true')";
        assert!(evaluate_condition(cond, &data).unwrap());
    }

    #[test]
    fn test_force_single_workstream_blocks_parallel() {
        let data = ctx(&[
            ("task_type", json!("Development")),
            ("workstream_count", json!(2)),
            ("force_single_workstream", json!(true)),
            ("recursion_guard", json!("ALLOWED")),
        ]);

        let cond = "('Development' in task_type or 'Investigation' in task_type) and workstream_count != '1' and workstream_count != '' and 'ALLOWED' in recursion_guard and force_single_workstream != 'true'";
        assert!(!evaluate_condition(cond, &data).unwrap());
    }

    #[test]
    fn test_force_single_workstream_false_allows_parallel() {
        let data = ctx(&[
            ("task_type", json!("Development")),
            ("workstream_count", json!(2)),
            ("force_single_workstream", json!("false")),
            ("recursion_guard", json!("ALLOWED")),
        ]);

        let cond = "('Development' in task_type or 'Investigation' in task_type) and workstream_count != '1' and workstream_count != '' and 'ALLOWED' in recursion_guard and force_single_workstream != 'true'";
        assert!(evaluate_condition(cond, &data).unwrap());
    }

    // -- Basic condition tests --

    #[test]
    fn test_string_equality() {
        let data = ctx(&[("status", json!("ok"))]);
        assert!(evaluate_condition("status == 'ok'", &data).unwrap());
        assert!(!evaluate_condition("status == 'fail'", &data).unwrap());
    }

    #[test]
    fn test_number_string_cross_type() {
        let data = ctx(&[("count", json!(1))]);
        assert!(evaluate_condition("count == '1'", &data).unwrap());
    }

    #[test]
    fn test_in_operator() {
        let data = ctx(&[("task_type", json!("Development"))]);
        assert!(evaluate_condition("'Development' in task_type", &data).unwrap());
        assert!(!evaluate_condition("'Q&A' in task_type", &data).unwrap());
    }

    #[test]
    fn test_boolean_and_or() {
        let data = ctx(&[("a", json!("x")), ("b", json!("y"))]);
        assert!(evaluate_condition("a == 'x' and b == 'y'", &data).unwrap());
        assert!(evaluate_condition("a == 'x' or b == 'z'", &data).unwrap());
        assert!(!evaluate_condition("a == 'z' and b == 'y'", &data).unwrap());
    }

    #[test]
    fn test_compare_values_bool_string_ordering() {
        let data = ctx(&[("flag", json!(true)), ("off", json!(false))]);
        assert!(evaluate_condition("flag >= 'true'", &data).unwrap());
        assert!(evaluate_condition("off <= 'false'", &data).unwrap());
        assert!(evaluate_condition("flag > 'false'", &data).unwrap());
        assert!(evaluate_condition("off < 'true'", &data).unwrap());
    }

    #[test]
    fn test_compare_values_bool_number_ordering() {
        let data = ctx(&[("flag", json!(true)), ("off", json!(false))]);
        assert!(evaluate_condition("flag >= 1", &data).unwrap());
        assert!(evaluate_condition("off <= 0", &data).unwrap());
        assert!(evaluate_condition("flag > 0", &data).unwrap());
        assert!(!evaluate_condition("off > 0", &data).unwrap());
    }

    #[test]
    fn test_compare_values_bool_bool_ordering() {
        let data = ctx(&[("a", json!(true)), ("b", json!(false))]);
        assert!(evaluate_condition("a > b", &data).unwrap());
        assert!(evaluate_condition("b < a", &data).unwrap());
        assert!(evaluate_condition("a >= a", &data).unwrap());
        assert!(evaluate_condition("b <= b", &data).unwrap());
    }

    #[test]
    fn test_bracket_access_object_key() {
        let data = ctx(&[("scope", json!({"has_ambiguities": true}))]);
        assert!(evaluate_condition("scope['has_ambiguities']", &data).unwrap());
        assert!(evaluate_condition("scope['has_ambiguities'] == 'true'", &data).unwrap());
    }

    #[test]
    fn test_bracket_access_works_in_legacy_guard_expression() {
        let data = ctx(&[("scope", json!({"has_ambiguities": true}))]);
        assert!(evaluate_condition("scope and scope['has_ambiguities']", &data).unwrap());
    }

    #[test]
    fn test_bracket_access_array_index() {
        let data = ctx(&[("items", json!(["alpha", "beta"]))]);
        assert!(evaluate_condition("items[1] == 'beta'", &data).unwrap());
    }
}
