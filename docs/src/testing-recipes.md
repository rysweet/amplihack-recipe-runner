# Testing & Edge-Case Recipes

Recipes designed to exercise specific recipe runner features and edge cases.
Useful as regression tests and as references for condition syntax.

Source: [recipes/testing/](https://github.com/rysweet/amplihack-recipe-runner/tree/main/recipes/testing)

## Recipes

| Recipe | What It Tests |
|--------|---------------|
| [all-condition-operators](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/all-condition-operators.yaml) | Every comparison and boolean operator: `==`, `!=`, `<`, `<=`, `>`, `>=`, `and`, `or`, `not`, `in`, `not in` |
| [all-functions](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/all-functions.yaml) | All whitelisted functions: `int()`, `str()`, `len()`, `bool()`, `float()`, `min()`, `max()` |
| [all-methods](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/all-methods.yaml) | All whitelisted string methods: `strip()`, `lstrip()`, `rstrip()`, `lower()`, `upper()`, `title()`, `startswith()`, `endswith()`, `replace()`, `split()`, `join()`, `count()`, `find()` |
| [output-chaining](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/output-chaining.yaml) | Step output stored in context and referenced by subsequent steps via `{{variable}}` |
| [json-extraction-strategies](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/json-extraction-strategies.yaml) | All 3 JSON extraction strategies: direct parse, markdown fence, balanced braces |
| [step-type-inference](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/step-type-inference.yaml) | Automatic step type detection: bash (command), agent (agent field), recipe (recipe field), agent (prompt-only) |
| [continue-on-error-chain](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/continue-on-error-chain.yaml) | `continue_on_error: true` allowing subsequent steps to run after failures |
| [nested-context](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/nested-context.yaml) | Dot-notation access to nested context values: `{{config.database.host}}` |
| [large-context](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/large-context.yaml) | Many context variables and long values to test template rendering at scale |
| [empty-and-edge-cases](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/empty-and-edge-cases.yaml) | Empty strings, missing variables, whitespace-only values, special characters |
