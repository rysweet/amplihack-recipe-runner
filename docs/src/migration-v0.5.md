# Migration to 0.5.0

Version 0.5.0 switches recipe parsing from the archived `serde_yaml` package to
the maintained `serde_norway` fork. YAML recipe syntax, CLI flags, and command
behavior remain unchanged.

The Rust library's public `parser::ParseError::Yaml` variant now contains
`serde_norway::Error`. Most callers that handle the variant without naming its
payload need no changes. If your code names or matches the old concrete
`serde_yaml::Error` type, add `serde_norway` as a direct dependency and update
that type path to `serde_norway::Error`.

The crate retains the `serde_yaml` dependency alias internally so existing
parser and serializer call sites remain stable. The alias does not preserve the
old external error type identity.
