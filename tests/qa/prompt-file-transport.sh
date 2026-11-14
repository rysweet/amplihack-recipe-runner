#!/usr/bin/env bash
# Preserve the QA entry point while exercising live wrapper contracts in Rust.
set -euo pipefail
cd "$(dirname "$0")/../.."
exec cargo test --test prompt_transport
