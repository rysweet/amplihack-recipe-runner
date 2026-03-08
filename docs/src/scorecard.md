# Feature Scorecard: Python ↔ Rust Recipe Runner Parity

Last updated: 2025-07-17

## Summary

| Metric | Value |
|---|---|
| Core Features (Python parity) | 24 |
| Ported to Rust | 24 |
| Parity | 100% |
| Rust-only Features | 12 |
| Total Features | 36 |
| Unit Tests | 53 |
| Integration Tests | 14 |
| Recipe Tests | 91 |
| Feature Tests | 25 |
| **Total Tests** | **183** |

## Core Feature Scorecard (Python Parity)

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

## Rust-only Features (Beyond Parity)

| Feature | Has Test | Status | Notes |
|---|---|---|---|
| Recipe-level recursion limits | ✅ | ✅ | max_depth + max_total_steps via RecursionConfig |
| Timeout enforcement (agent steps) | ✅ | ✅ | Heartbeat deadline + SIGTERM/SIGKILL |
| continue_on_error step flag | ✅ | ✅ | Per-step failure tolerance |
| Adapter fallback chain | ✅ | ✅ | FallbackAdapter<P, S> generic composition |
| CLI subcommands (run, list) | ✅ | ✅ | --validate-only, --explain, --progress, --output-format json |
| Tag filtering (include/exclude) | ✅ | ✅ | when_tags on steps + --include-tags/--exclude-tags |
| JSONL audit log | ✅ | ✅ | Per-run audit file with step_id, status, duration_ms |
| Pre/post/on_error hooks | ✅ | ✅ | RecipeHooks with shell command execution |
| Discovery caching (TTL) | ✅ | ✅ | DiscoveryCache with 30s default TTL |
| Parallel step execution | ✅ | ✅ | parallel_group with std::thread::scope |
| Recipe composition (extends) | ✅ | ✅ | Single-level recipe inheritance |
| Property-based testing (proptest) | ✅ | ✅ | Fuzz condition evaluator, templates, YAML parser |

## Legend

- ✅ = Complete and passing
- 🔄 = Partial implementation
- ⬜ = Not yet started
- ❌ = Failed
