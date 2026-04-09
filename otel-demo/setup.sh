#!/usr/bin/env bash
set -euo pipefail

# ─────────────────────────────────────────────────────────
# setup.sh — End-to-end setup for mssql-tds OTel demo
#
# Brings up SQL Server + OTel Collector + Prometheus + Grafana,
# builds the demo app, runs a few queries, and prints URLs.
#
# Prerequisites: Docker (with compose), Rust toolchain (cargo)
# ─────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DB_PASSWORD="${DB_PASSWORD:-StrongP@ss1}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info()  { echo -e "${GREEN}[✓]${NC} $*"; }
warn()  { echo -e "${YELLOW}[!]${NC} $*"; }
err()   { echo -e "${RED}[✗]${NC} $*"; }

# ── Preflight ──────────────────────────────────────────
echo "═══════════════════════════════════════════════════"
echo "  mssql-tds OTel Demo — Setup"
echo "═══════════════════════════════════════════════════"
echo

# Check Docker
if ! command -v docker &>/dev/null; then
    err "Docker not found. Install it: https://docs.docker.com/get-docker/"
    exit 1
fi
if ! docker compose version &>/dev/null && ! docker-compose version &>/dev/null; then
    err "Docker Compose not found."
    exit 1
fi
info "Docker found"

# Check Rust
if ! command -v cargo &>/dev/null; then
    err "Rust toolchain not found. Install it: https://rustup.rs"
    exit 1
fi
info "Rust toolchain found ($(rustc --version))"

# ── Start infrastructure ───────────────────────────────
echo
info "Starting Docker containers (SQL Server, OTel Collector, Prometheus, Grafana)..."
cd "$SCRIPT_DIR"
DB_PASSWORD="$DB_PASSWORD" docker compose up -d

# ── Wait for SQL Server ────────────────────────────────
info "Waiting for SQL Server to be ready..."
RETRIES=30
until docker compose exec -T sqlserver /opt/mssql-tools18/bin/sqlcmd \
        -S localhost -U sa -P "$DB_PASSWORD" -C -Q "SELECT 1" &>/dev/null; do
    RETRIES=$((RETRIES - 1))
    if [ "$RETRIES" -le 0 ]; then
        err "SQL Server did not become ready in time"
        docker compose logs sqlserver | tail -20
        exit 1
    fi
    sleep 2
done
info "SQL Server is ready"

# ── Build the demo app ─────────────────────────────────
echo
info "Building demo app (cargo build -p otel-demo)..."
cd "$REPO_ROOT"
cargo build -p otel-demo 2>&1 | tail -5
info "Build complete"

# ── Run the demo app ──────────────────────────────────
echo
info "Starting demo app (background)..."
DB_PASSWORD="$DB_PASSWORD" cargo run -p otel-demo &
APP_PID=$!

# Wait for app to start
sleep 3
if ! kill -0 "$APP_PID" 2>/dev/null; then
    err "Demo app failed to start"
    exit 1
fi
info "Demo app running (PID $APP_PID)"

# ── Generate some traffic ─────────────────────────────
echo
info "Generating sample traffic..."
for i in $(seq 1 5); do
    curl -sf http://localhost:3002/query >/dev/null && echo -n "." || echo -n "x"
done
echo
for i in $(seq 1 3); do
    curl -sf http://localhost:3002/heavy >/dev/null && echo -n "." || echo -n "x"
done
echo
info "Traffic generated"

# ── Print summary ─────────────────────────────────────
echo
echo "═══════════════════════════════════════════════════"
echo "  Setup complete! Here's what's running:"
echo "═══════════════════════════════════════════════════"
echo
echo "  Demo App:       http://localhost:3002"
echo "    /query        Run SELECT @@VERSION"
echo "    /heavy        Run cross-join (1000 rows)"
echo "    /health       Health check"
echo "    /metrics      Prometheus metrics endpoint"
echo
echo "  Prometheus:     http://localhost:9090"
echo "  Grafana:        http://localhost:3001  (admin/admin)"
echo "    Dashboard:    TDS Network I/O (auto-provisioned)"
echo
echo "  SQL Server:     localhost:1433  (sa / $DB_PASSWORD)"
echo
echo "═══════════════════════════════════════════════════"
echo "  To stop everything:"
echo "    kill $APP_PID"
echo "    cd $SCRIPT_DIR && docker compose down"
echo "═══════════════════════════════════════════════════"
