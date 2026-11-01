#!/usr/bin/env bash
# Starts the whole workspace for end-to-end checks, on its own database
# (ao_e2e, recreated each time) and its own ports, with the gateway's MOCK
# provider: no real model is ever called and no money is spent.
#
#   scripts/e2e-stack.sh start   build, recreate ao_e2e, start everything
#   scripts/e2e-stack.sh stop    stop what start started
#
# Reads the development .env. Each process gets only the variables it needs,
# so the engine and the server never see provider keys or the master key.
# Nothing here prints a secret. Logs go to target/e2e/.
#
# Ports: web 3100, server 8190, gateway 8181.
set -euo pipefail
cd "$(dirname "$0")/.."

DIR=target/e2e
WEB_PORT=3100
SERVER_PORT=8190
GATEWAY_PORT=8181
E2E_DB=ao_e2e
ADMIN_EMAIL=admin@e2e.test
# A throwaway account in a throwaway database, used only by the e2e tests.
ADMIN_PASSWORD="e2e administrator passphrase"

stop() {
  if [ -f "$DIR/pids" ]; then
    # Each process leads its own process group (setsid), so its children
    # (the Next.js server under npx) stop with it.
    while read -r pid; do
      kill -- "-$pid" 2>/dev/null || kill "$pid" 2>/dev/null || true
    done < "$DIR/pids"
    rm -f "$DIR/pids"
  fi
}

# Replaces the database name at the end of a postgres URL.
on_e2e_db() {
  local url=$1
  case "$url" in
    *\?*) echo "database URLs with parameters are not supported here" >&2; exit 1 ;;
  esac
  printf '%s/%s' "${url%/*}" "$E2E_DB"
}

wait_for() {
  local url=$1 name=$2
  for _ in $(seq 1 120); do
    if curl -s -o /dev/null "$url"; then
      return 0
    fi
    sleep 1
  done
  echo "$name did not start; see $DIR/$name.log" >&2
  exit 1
}

start() {
  stop
  mkdir -p "$DIR"
  set -a
  # shellcheck disable=SC1091
  . ./.env
  set +a
  local base_path="/usr/local/bin:/usr/bin:/bin:$HOME/.cargo/bin"
  local admin_db gateway_db server_db engine_db
  admin_db=$(on_e2e_db "$DATABASE_URL")
  gateway_db=$(on_e2e_db "$GATEWAY_DATABASE_URL")
  server_db=$(on_e2e_db "$SERVER_DATABASE_URL")
  engine_db=$(on_e2e_db "$ENGINE_DATABASE_URL")

  cargo build -q -p gateway -p server -p engine --locked

  psql -q -v ON_ERROR_STOP=1 "$DATABASE_URL" -c "DROP DATABASE IF EXISTS $E2E_DB WITH (FORCE)" >/dev/null
  psql -q -v ON_ERROR_STOP=1 "$DATABASE_URL" -c "CREATE DATABASE $E2E_DB" >/dev/null
  env -i PATH="$base_path" ADMIN_DATABASE_URL="$admin_db" \
    GATEWAY_DB_PASSWORD="$GATEWAY_DB_PASSWORD" ENGINE_DB_PASSWORD="$ENGINE_DB_PASSWORD" \
    SERVER_DB_PASSWORD="$SERVER_DB_PASSWORD" \
    target/debug/server migrate > "$DIR/migrate.log" 2>&1
  printf '%s\n' "$ADMIN_PASSWORD" | env -i PATH="$base_path" ADMIN_DATABASE_URL="$admin_db" \
    target/debug/server create-admin "$ADMIN_EMAIL" "Administradora" > "$DIR/admin.log" 2>&1

  : > "$DIR/pids"
  env -i PATH="$base_path" RUST_LOG=info GATEWAY_PROVIDER=mock \
    GATEWAY_BIND="127.0.0.1:$GATEWAY_PORT" DATABASE_URL="$gateway_db" \
    GATEWAY_MASTER_KEY="$GATEWAY_MASTER_KEY" GATEWAY_ISSUER_SECRET="$GATEWAY_ISSUER_SECRET" \
    GATEWAY_ADMIN_SECRET="$GATEWAY_ADMIN_SECRET" GATEWAY_MONTHLY_BUDGET_USD="$GATEWAY_MONTHLY_BUDGET_USD" \
    setsid target/debug/gateway > "$DIR/gateway.log" 2>&1 &
  echo $! >> "$DIR/pids"
  env -i PATH="$base_path" RUST_LOG=info SERVER_BIND="127.0.0.1:$SERVER_PORT" \
    SERVER_DATABASE_URL="$server_db" SERVER_PUBLIC_ORIGIN="http://localhost:$WEB_PORT" \
    GATEWAY_ADMIN_SECRET="$GATEWAY_ADMIN_SECRET" SERVER_GATEWAY_URL="http://127.0.0.1:$GATEWAY_PORT" \
    setsid target/debug/server serve > "$DIR/server.log" 2>&1 &
  echo $! >> "$DIR/pids"
  env -i PATH="$base_path" RUST_LOG=info DATABASE_URL="$engine_db" \
    GATEWAY_ISSUER_SECRET="$GATEWAY_ISSUER_SECRET" ENGINE_GATEWAY_URL="http://127.0.0.1:$GATEWAY_PORT" \
    setsid target/debug/engine worker > "$DIR/worker.log" 2>&1 &
  echo $! >> "$DIR/pids"

  (
    cd web
    env -i PATH="$base_path:$(dirname "$(command -v node)")" HOME="$HOME" NEXT_TELEMETRY_DISABLED=1 \
      SERVER_API_URL="http://127.0.0.1:$SERVER_PORT" npx next build > "../$DIR/web-build.log" 2>&1
    env -i PATH="$base_path:$(dirname "$(command -v node)")" HOME="$HOME" NEXT_TELEMETRY_DISABLED=1 \
      SERVER_API_URL="http://127.0.0.1:$SERVER_PORT" setsid npx next start -H 127.0.0.1 -p "$WEB_PORT" \
      > "../$DIR/web.log" 2>&1 &
    echo $! >> "../$DIR/pids"
  )

  wait_for "http://127.0.0.1:$GATEWAY_PORT/health" gateway
  wait_for "http://127.0.0.1:$SERVER_PORT/api/me" server
  wait_for "http://127.0.0.1:$WEB_PORT/login" web
  echo "e2e stack ready: http://localhost:$WEB_PORT (mock provider, database $E2E_DB)"
}

case "${1:-}" in
  start) start ;;
  stop) stop ;;
  *) echo "usage: $0 start|stop" >&2; exit 64 ;;
esac
