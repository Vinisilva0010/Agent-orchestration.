#!/usr/bin/env bash
# Runs the browser end-to-end tests: starts the workspace with the mock
# provider (scripts/e2e-stack.sh), runs Playwright inside its official image
# (pinned by digest, so no browser or system library is installed on the
# host), then stops the workspace. No real model is ever called.
set -euo pipefail
cd "$(dirname "$0")/.."

IMAGE="mcr.microsoft.com/playwright:v1.64.1-noble@sha256:2f05f8a734509a7b3f26a912639488ee08f58a7b292d14d62271bcff9038768f"

scripts/e2e-stack.sh start
trap 'scripts/e2e-stack.sh stop' EXIT
mkdir -p target/e2e/results
docker run --rm --network host --user "$(id -u):$(id -g)" \
  -e HOME=/tmp -e E2E_OUTPUT_DIR=/results/run -e CI=1 \
  -v "$PWD/web:/web:ro" -v "$PWD/target/e2e/results:/results" -w /web \
  "$IMAGE" node node_modules/@playwright/test/cli.js test "$@"
