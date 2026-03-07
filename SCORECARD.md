# Feature Scorecard: Python ↔ Rust Recipe Runner Parity

Last updated: 2026-03-07

## Summary

| Metric | Value |
|---|---|
| Total Features | 15 |
| Ported to Rust | 12 |
| Parity | 80% |

## Detailed Scorecard

| Feature | Has Test | Passes (Python) | Passes (Rust) | Notes |
|---|---|---|---|---|
| YAML recipe parsing | ✅ | ✅ | ✅ | serde_yaml |
| Step type inference | ✅ | ✅ | ✅ | bash/agent/recipe |
| Duplicate step ID detection | ✅ | ✅ | ✅ | |
| File size limit (1MB) | ✅ | ✅ | ✅ | |
| Template rendering `{{var}}` | ✅ | ✅ | ✅ | regex-based |
| Shell-escaped rendering | ✅ | ✅ | ✅ | shell-escape crate |
| Dot-notation context access | ✅ | ✅ | ✅ | a.b.c |
| Safe condition evaluation | ✅ | ✅ | ✅ | Recursive descent parser |
| JSON extraction (3 strategies) | ✅ | ✅ | ✅ | direct/fence/balanced |
| Conditional step execution | ✅ | ✅ | ✅ | |
| Dry run mode | ✅ | ✅ | ✅ | |
| CLI subprocess adapter | ✅ | ✅ | ✅ | |
| Sub-recipe execution | ✅ | ✅ | 🔄 | Basic (no full context merge) |
| Agent resolver | ✅ | ✅ | ⬜ | Not yet ported |
| Recipe discovery | ✅ | ✅ | 🔄 | Basic file search only |

## Legend

- ✅ = Complete and passing
- 🔄 = Partial implementation
- ⬜ = Not yet started
- ❌ = Failed
