#!/bin/bash
cargo test --lib 2>&1 | grep -E "test condition::tests::" | head -50
