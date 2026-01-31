#!/bin/bash

# Setup git hooks for rstmdb development
# Run this once after cloning the repo

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(dirname "$SCRIPT_DIR")"

cd "$REPO_DIR"

# Configure git to use our hooks directory
git config core.hooksPath .githooks

echo "Git hooks configured!"
echo "Pre-commit hook will run: fmt check, clippy, tests"
