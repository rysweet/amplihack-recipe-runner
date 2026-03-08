# Condition Language Reference

The recipe runner's condition evaluator is a hand-rolled **tokenizer + recursive-descent parser** implemented in `src/context.rs`. Conditions are expressions evaluated to determine if a step should execute. If the condition evaluates to truthy, the step runs; otherwise it's skipped.

If evaluation itself fails (e.g., a syntax error), the step is marked **Failed** — not skipped.

---

## Truthiness

| Type            | Truthy                     | Falsy              |
| --------------- | -------------------------- | ------------------ |
| Boolean         | `true`                     | `false`            |
| Number          | Any non-zero (e.g., `1`, `-3.14`) | `0`, `0.0`   |
| String          | Non-empty (e.g., `"hello"`) | Empty string `""`  |
| Array           | Non-empty                  | Empty `[]`         |
| Object          | Non-empty                  | Empty `{}`         |
| Null            | —                          | Always falsy       |

---

## Operators

Listed by precedence, lowest to highest:

| Precedence | Operator   | Kind         | Description                              |
| ---------- | ---------- | ------------ | ---------------------------------------- |
| 1 (lowest) | `or`       | Logical      | Short-circuit logical OR                 |
| 2          | `and`      | Logical      | Short-circuit logical AND                |
| 3          | `not`      | Unary        | Logical negation (prefix)                |
| 4 (highest)| `==`       | Comparison   | Equality (with type coercion)            |
|            | `!=`       | Comparison   | Inequality                               |
|            | `<`        | Comparison   | Less than                                |
|            | `<=`       | Comparison   | Less than or equal                       |
|            | `>`        | Comparison   | Greater than                             |
|            | `>=`       | Comparison   | Greater than or equal                    |
|            | `in`       | Membership   | Substring or array membership            |
|            | `not in`   | Membership   | Negated membership (parsed as one token) |

### Type coercion in comparisons

- **Equality** (`==`, `!=`): Same types compare directly. Mixed types fall back to comparing string representations (so `5 == "5"` is `true`).
- **Ordering** (`<`, `<=`, `>`, `>=`): Number–Number is numeric. String–String is lexicographic. String–Number attempts to parse the string as `f64` then compares numerically. All other combinations are incomparable (condition evaluates as falsy).
- **Membership** (`in`, `not in`): Against a string, checks substring containment. Against an array, checks element equality via `values_equal`. Against any other type, evaluates as falsy.

---

## Literals

| Type    | Syntax                          | Notes                                      |
| ------- | ------------------------------- | ------------------------------------------ |
| String  | `"hello"` or `'world'`         | Single or double quotes. Backslash escapes supported (`\'`, `\"`). |
| Number  | `42`, `3.14`, `-7`             | All parsed and stored as `f64`.            |
| Boolean | `true`, `True`, `false`, `False` | Case-sensitive to these exact forms.       |
| None    | `none`                          | Not a keyword — it's an unknown identifier that resolves to `Null`. |

---

## Identifiers

Identifiers are alphanumeric names (plus underscores) that look up values in the recipe context.

| Form             | Example          | Behavior                                        |
| ---------------- | ---------------- | ----------------------------------------------- |
| Simple           | `my_var`         | Looks up `my_var` in the top-level context.     |
| Dot-notation     | `result.status`  | Nested lookup: `context["result"]["status"]`.   |
| Unknown          | `undefined_var`  | Resolves to `Null` (falsy). No error raised.    |

Dot-notation in identifiers is resolved during parsing — each segment walks one level deeper into nested JSON values. If any segment is missing, the whole expression resolves to `Null`.

---

## Function Calls

Only whitelisted function names are allowed. Calling an unknown function is an error.

| Function         | Signature        | Description                                                              |
| ---------------- | ---------------- | ------------------------------------------------------------------------ |
| `int(value)`     | 1 arg            | Convert to integer (i64). Strings are parsed, bools → 0/1, else 0.      |
| `float(value)`   | 1 arg            | Convert to f64. Strings are parsed, bools → 0.0/1.0, else 0.0.          |
| `str(value)`     | 1 arg            | Convert to string. Null → `""`. Numbers use serde's `to_string()`.      |
| `bool(value)`    | 1 arg            | Convert to boolean using the truthiness rules above.                     |
| `len(value)`     | 1 arg            | Length of string (bytes), array, or object. Other types return 0.        |
| `min(a, b, ...)` | 2+ args          | Minimum of values (uses ordering comparison). Requires at least 2 args.  |
| `max(a, b, ...)` | 2+ args          | Maximum of values (uses ordering comparison). Requires at least 2 args.  |

---

## Method Calls

Methods use `.method(args)` syntax and can only be called on **string** values. Calling a method on a non-string is an error. Only whitelisted method names are allowed.

| Method                    | Returns  | Description                                                  |
| ------------------------- | -------- | ------------------------------------------------------------ |
| `.strip()`                | String   | Trim whitespace from both ends.                              |
| `.lstrip()`               | String   | Trim whitespace from the left (start).                       |
| `.rstrip()`               | String   | Trim whitespace from the right (end).                        |
| `.lower()`                | String   | Convert to lowercase.                                        |
| `.upper()`                | String   | Convert to uppercase.                                        |
| `.title()`                | String   | Title-case each whitespace-separated word.                   |
| `.startswith(prefix)`     | Boolean  | True if string starts with `prefix`.                         |
| `.endswith(suffix)`       | Boolean  | True if string ends with `suffix`.                           |
| `.replace(old, new)`      | String   | Replace all occurrences of `old` with `new`.                 |
| `.split(sep)`             | Array    | Split by `sep`. If no arg, splits on whitespace.             |
| `.join(arr)`              | String   | Join array elements with the string as separator.            |
| `.count(sub)`             | Number   | Count non-overlapping occurrences of `sub`.                  |
| `.find(sub)`              | Number   | Index of first occurrence of `sub`. Returns `-1` if not found. |

Methods can be chained: `name.strip().lower()`.

---

## Safety Features

1. **Whitelist-only execution** — Only the functions and methods listed above are allowed. Unknown names produce an error, not silent null.
2. **Dunder blocking** — Any expression containing `__` (e.g., `__class__`, `__import__`) is rejected before parsing even begins.
3. **No assignment, no side effects** — The expression language is pure; it can only read context values and compute results.
4. **Unknown identifiers are null** — Referencing a variable that doesn't exist returns `Null` (falsy) rather than raising an error. This is intentional for optional-variable patterns.

---

## Important Gotchas

### 1. All numbers are f64

Numbers are parsed and stored as `f64` internally. This means `str(42)` produces `"42.0"`, not `"42"`. If you need the integer string representation, store it as a string in the context instead of using `str()` on a numeric literal.

### 2. `shell_escape::escape()` wraps values in single quotes

When using `render_shell()` for template expansion, empty strings become `''` (two single quotes), not the empty string. This is correct for shell safety but may surprise you in conditions that check the rendered result.

### 3. Unknown identifiers are null by design

This is a feature, not a bug. It allows patterns like `condition: "optional_var"` to work — if `optional_var` isn't set, the condition is falsy and the step is skipped without error.

### 4. `not in` is a single operator

The tokenizer uses lookahead to parse `not in` as one token (`NotIn`), distinct from a standalone `not` followed by `in`. This means `not in` always means "not contained in", never "negation of the result of `in`" — though the result is the same.

### 5. Boolean keywords are case-sensitive

Only `true`/`True` and `false`/`False` are recognized. `TRUE`, `FALSE`, `tRue`, etc. are treated as regular identifiers and will resolve to `Null`.

### 6. `none` is not a keyword

There is no `none` or `None` literal. Writing `none` creates an identifier lookup that (typically) resolves to `Null` because no context variable named `none` exists. This works in practice but is not guaranteed if someone sets a context variable called `none`.

---

## Examples

### Basic truthiness

```yaml
# Truthy if 'analysis' is set and non-empty in context
condition: "analysis"

# Always true
condition: "true"

# Always false
condition: "false"
```

### String comparison

```yaml
# Exact match
condition: "status == 'success'"

# Not equal
condition: "status != 'error'"

# Case-insensitive comparison via method
condition: "status.lower() == 'success'"
```

### Numeric comparison

```yaml
# Greater than
condition: "count > 0"

# Compound range check
condition: "count > 0 and count < 10"

# With function conversion
condition: "int(exit_code) == 0"
```

### Logical operators

```yaml
# Negation
condition: "not skip_tests"

# AND
condition: "has_tests and not skip_tests"

# OR
condition: "use_cache or force_rebuild"

# Combined with parentheses
condition: "(status == 'success' or status == 'partial') and not skip"
```

### Membership tests

```yaml
# Substring containment
condition: "'error' in output"

# Negated containment
condition: "'error' not in output"

# Array membership (items is an array in context)
condition: "'admin' in roles"
```

### Function calls

```yaml
# Length check
condition: "len(items) > 0"

# Type conversion
condition: "int(retry_count) < 3"

# Boolean conversion
condition: "bool(result)"

# Min/max
condition: "max(score_a, score_b) >= 80"
```

### Method calls

```yaml
# String prefix check
condition: "name.startswith('test_')"

# String suffix check
condition: "filename.endswith('.py')"

# Chained methods
condition: "input.strip().lower() == 'yes'"

# Replace and check
condition: "path.replace('\\', '/').startswith('/home')"

# Split and check length
condition: "len(csv_line.split(',')) > 3"

# Find (returns index or -1)
condition: "message.find('WARNING') >= 0"

# Count occurrences
condition: "log_output.count('ERROR') == 0"
```

### Nested context access

```yaml
# Dot-notation for nested values
condition: "result.status == 'ok'"

# Deep nesting
condition: "response.data.count > 0"
```

### Optional variable patterns

```yaml
# Skip step if variable isn't set (resolves to null → falsy)
condition: "optional_feature"

# Guard with default-like logic
condition: "config.verbose and len(debug_output) > 0"
```

### Cross-type equality

```yaml
# Number-string coercion: this is true if exit_code is 0 (the number)
condition: "exit_code == '0'"

# But be careful: str(42) gives "42.0", not "42"
# So this does NOT work as expected:
#   condition: "str(count) == '42'"    # produces "42.0" == "42" → false
```
