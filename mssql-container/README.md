# mssql-container

Pull and run SQL Server Linux containers **without Docker** or any container runtime installed. Uses OCI image pulling + Linux namespaces directly.

## How It Works

1. **Pulls the image** from `mcr.microsoft.com` using the OCI Distribution / Docker Registry v2 HTTP API (anonymous auth)
2. **Extracts layers** into a local rootfs, handling whiteouts per the OCI spec
3. **Creates a container** using Linux namespaces (`CLONE_NEWPID`, `CLONE_NEWNS`, `CLONE_NEWUTS`) with `pivot_root`
4. **Starts SQL Server** (`/opt/mssql/bin/sqlservr`) inside the namespace
5. **Health checks** by polling the TCP port until ready

## Requirements

- **Linux** with root privileges (or `CAP_SYS_ADMIN`)
- Internet access to pull from `mcr.microsoft.com`
- On Windows: WSL2 (stub — not yet implemented)

## Usage

```rust
use mssql_container::SqlServerContainer;

// Simple — defaults to mcr.microsoft.com/mssql/server:2022-latest
let container = SqlServerContainer::start().await?;
println!("Connect at: {}", container.connection_string());
// Use it...
drop(container); // auto-stops

// Configurable
let container = SqlServerContainer::builder()
    .image("mcr.microsoft.com/mssql/server")
    .tag("2022-latest")
    .sa_password("MyStr0ngP@ss!")
    .port(1434)
    .cache_dir("/custom/cache")
    .build()
    .start()
    .await?;

println!("Host: {}", container.host());       // "127.0.0.1"
println!("Port: {}", container.port());        // 1434
println!("Password: {}", container.sa_password());
println!("URL: {}", container.url());
```

## Caching

OCI layer blobs are cached in `~/.mssql-container/cache/` by default. Assembled rootfs images are stored in `~/.mssql-container/rootfs/<hash>/`. Subsequent runs skip the download and extraction steps if the cache is intact.

## Architecture

| Module | Purpose |
|--------|---------|
| `lib.rs` | Public API (`SqlServerContainer`, builder) |
| `registry.rs` | OCI registry client (manifest + blob pull) |
| `image.rs` | Layer extraction, rootfs assembly, caching |
| `container.rs` | Linux namespace setup, `pivot_root`, process management |
| `wsl.rs` | Windows WSL2 wrapper (stub) |
| `error.rs` | Error types |

## License

MIT
