# Feature Scorecard: Python ↔ Rust Recipe Runner Parity

Last updated: 2025-07-17

## Summary

| Metric | Value |
|---|---|
| Total Features | 24 |
| Ported to Rust | 24 |
| Parity | 100% |
| Unit Tests | 46 |
| Integration Tests | 14 |
| Total Tests | 60 |

## Detailed Scorecard

| Feature | Has Test | Passes (Python) | Passes (Rust) | Notes |
|---|---|---|---|---|
| YAML recipe parsing | ✅ | ✅ | ✅ | serde_yaml |
| Step type inference | ✅ | ✅ | ✅ | bash/agent/recipe with priority rules |
| Duplicate step ID detection | ✅ | ✅ | ✅ | |
| File size limit (1MB) | ✅ | ✅ | ✅ | |
| Template rendering `{{var}}` | ✅ | ✅ | ✅ | regex-based |
| Shell-escaped rendering | ✅ | ✅ | ✅ | shell-escape crate |
| Dot-notation context access | ✅ | ✅ | ✅ | a.b.c |
| Safe condition evaluation | ✅ | ✅ | ✅ | Recursive descent parser |
| Comparison operators (<, <=, >, >=) | ✅ | ✅ | ✅ | |
| Safe function calls (int/str/len/bool/float/min/max) | ✅ | ✅ | ✅ | Matches _SAFE_CALL_NAMES |
| Safe method calls (strip/lower/upper/startswith/etc) | ✅ | ✅ | ✅ | Matches _SAFE_METHOD_NAMES |
| JSON extraction (3 strategies) | ✅ | ✅ | ✅ | direct/fence/balanced |
| JSON retry mechanism | ✅ | ✅ | ✅ | Re-prompts with JSON reminder |
| Conditional step execution | ✅ | ✅ | ✅ | |
| Dry run mode | ✅ | ✅ | ✅ | Mock JSON for parse_json steps |
| Auto-stage git changes | ✅ | ✅ | ✅ | After agent steps |
| Fail-fast behavior | ✅ | ✅ | ✅ | Stops on first failure |
| CLI subprocess adapter | ✅ | ✅ | ✅ | Full feature parity |
| Non-interactive footer | ✅ | ✅ | ✅ | Prevents hanging |
| Temp dir isolation | ✅ | ✅ | ✅ | Prevents file races (#2758) |
| Session tree env vars | ✅ | ✅ | ✅ | AMPLIHACK_TREE_ID, DEPTH, MAX_DEPTH |
| Output streaming + heartbeat | ✅ | ✅ | ✅ | Background thread tails log |
| Sub-recipe execution | ✅ | ✅ | ✅ | Context merge, depth guard, recursion |
| Agent resolver | ✅ | ✅ | ✅ | Path traversal protection |
| Recipe discovery | ✅ | ✅ | ✅ | Multi-dir, SHA-256 manifest, sync |
| Unrecognized field detection | ✅ | ✅ | ✅ | Typo warnings for top-level + step fields |
| Public API (run_recipe_by_name) | ✅ | ✅ | ✅ | Convenience functions |

## Legend

- ✅ = Complete and passing
- 🔄 = Partial implementation
- ⬜ = Not yet started
- ❌ = Failed
