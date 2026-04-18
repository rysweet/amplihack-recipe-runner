#!/bin/bash
set -e

cat > /tmp/trailing_fn_test.rs << 'RUSTEOF'
#[cfg(test)]
mod trailing_fn_tests {
    use crate::condition::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn ctx(pairs: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn test_function_call_trailing_comma_should_work() {
        // int() function with trailing comma - should this work like list literals?
        let data = ctx(&[]);
        let result = evaluate_condition("int('5',) == 5", &data);
        println!("int('5',) result: {:?}", result);
        // Currently errors - is this inconsistent with list literals?
    }

    #[test]
    fn test_method_call_trailing_comma_should_work() {
        // Method call with trailing comma - should this work like list literals?
        let data = ctx(&[("s", json!("hello"))]);
        let result = evaluate_condition("s.upper(,)", &data);
        println!("s.upper(,) result: {:?}", result);
        // Currently errors - is this inconsistent with list literals?
    }
}
RUSTEOF

cat /tmp/trailing_fn_test.rs >> src/condition.rs
cargo test trailing_fn_tests -- --nocapture 2>&1 | grep -A 3 "result:"
git checkout src/condition.rs 2>/dev/null || true
