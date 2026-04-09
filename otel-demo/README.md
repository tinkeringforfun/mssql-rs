# OTel Demo — mssql-tds Observability

Instruments the `mssql-tds` crate with OpenTelemetry metrics and visualizes them in Grafana.

## Architecture

```
Demo App (axum :3002) ──OTLP gRPC──▶ OTel Collector ──▶ Prometheus ──▶ Grafana (:3001)
       └── /metrics (prometheus) ────────────────────────────┘
       └── connects to ──▶ SQL Server (:1433)
```

## One-command setup

```bash
cd otel-demo
./setup.sh
```

This will:
1. Start SQL Server, OTel Collector, Prometheus, and Grafana via Docker Compose
2. Wait for SQL Server to be healthy
3. Build and run the demo app
4. Generate sample traffic
5. Print URLs for everything

**Prerequisites:** Docker (with compose plugin) and Rust toolchain (`cargo`).

## Manual setup

```bash
# 1. Start the infrastructure stack
cd otel-demo
docker compose up -d

# 2. Build & run the demo app (from workspace root)
cd ..
cargo run -p otel-demo

# 3. Generate traffic
curl http://localhost:3002/query
curl http://localhost:3002/heavy
curl http://localhost:3002/health

# 4. View metrics
# App prometheus:  http://localhost:3002/metrics
# Prometheus UI:   http://localhost:9090
# Grafana:         http://localhost:3001  (admin / admin)
#   Dashboard:     "TDS Network I/O" (auto-provisioned)
```

## Metrics emitted

- `tds.network.read.duration_ms` — Time per network `.receive()` call
- `tds.network.read.bytes` — Bytes read per `.receive()` call
- `tds.packet.read.duration_ms` — Total time to assemble one TDS packet

## Endpoints

- `GET /query` — Runs `SELECT @@VERSION`
- `GET /heavy` — Runs a heavy cross-join query (1000 rows)
- `GET /health` — Health check
- `GET /metrics` — Prometheus metrics

## Environment variables

- `DB_PASSWORD` — SQL Server SA password (default: `StrongP@ss1`)

## Teardown

```bash
# Stop the demo app (PID printed by setup.sh)
kill <PID>

# Stop infrastructure
cd otel-demo
docker compose down
```
