#!/usr/bin/env bash

set -e

# Build the project in release mode with the `prod` feature across the workspace
cargo build --release --features prod --workspace

# Ensure the target binary directory exists
DEST="$HOME/.local/bin"
mkdir -p "$DEST"

# Copy the compiled binary to the user’s local bin directory
cp target/release/github-commit-info "$DEST/"

echo "Successfully built and installed github-commit-info to $DEST/"
