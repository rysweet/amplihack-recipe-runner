#!/bin/bash
set -e

# Create a temporary test file
cat > /tmp/edge_test.rs << 'RUSTEOF'
#[cfg(test)]
mod edge_tests {
    use crate::condition::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn ctx(pairs: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn test_empty_list_only_comma() {
        let data = ctx(&[("x", json!("a"))]);
        let result = evaluate_condition("x in [,]", &data);
        assert!(result.is_err(), "Should error on [,] but got {:?}", result);
    }

    #[test]
    fn test_double_comma_in_list() {
        let data = ctx(&[("x", json!("a"))]);
        let result = evaluate_condition("x in ['a',,]", &data);
        assert!(result.is_err(), "Should error on double comma but got {:?}", result);
    }

    #[test]
    fn test_method_trailing_comma() {
        let data = ctx(&[("s", json!("hello"))]);
        let result = evaluate_condition("s.upper(,) == 'HELLO'", &data);
        assert!(result.is_err(), "Should error on method(,) but got {:?}", result);
    }

    #[test]
    fn test_function_trailing_comma() {
        let data = ctx(&[]);
        let result = evaluate_condition("int('5',) == 5", &data);
        assert!(result.is_err(), "Should error on int('5',) but got {:?}", result);
    }
}
RUSTEOF

# Append to condition.rs temporarily
cat /tmp/edge_test.rs >> src/condition.rs

# Run the tests
cargo test edge_tests 2>&1

# Restore
git checkout src/condition.rs 2>/dev/null || true
