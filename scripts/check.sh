#!/bin/sh
# Verification gate: run before calling any change complete. CI runs this
# same script (.github/workflows/rust.yml), so passing here means passing there.
#
# Usage: scripts/check.sh
#
# The duplication check (jscpd) runs separately in CI through super-linter;
# see AGENTS.md for the local command.
set -eu

cd "$(dirname "$0")/.."

# Any warning fails the gate, test code included. Dependencies are capped by
# cargo and never fail it.
export RUSTFLAGS="${RUSTFLAGS:-} -D warnings"

# `probe` builds shurectl-probe and `qa` the --qa harness; without them
# clippy and the build skip that code. The build also links shurectl-probe,
# which no test does.
cargo clippy --all-targets --features probe,qa
cargo build --all-targets --features probe,qa
cargo fmt --check
cargo test
cargo test --features qa --test e2e
