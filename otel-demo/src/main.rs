use axum::{routing::get, Router};
use mssql_tds::connection::client_context::ClientContext;
use mssql_tds::connection_provider::tds_connection_provider::TdsConnectionProvider;
use mssql_tds::core::EncryptionOptions;
use mssql_tds::core::EncryptionSetting;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use prometheus::{Encoder, TextEncoder};
use std::sync::Arc;

fn db_password() -> String {
    std::env::var("DB_PASSWORD").unwrap_or_else(|_| "StrongP@ss1".to_string())
}

async fn create_client() -> Result<mssql_tds::connection::tds_client::TdsClient, String> {
    let mut context = ClientContext::default();
    context.user_name = "sa".to_string();
    context.password = db_password();
    context.database = "master".to_string();
    context.encryption_options = EncryptionOptions {
        mode: EncryptionSetting::PreferOff,
        trust_server_certificate: true,
        host_name_in_cert: None,
        server_certificate: None,
    };

    let provider = TdsConnectionProvider::new();
    provider
        .create_client(context, "tcp:localhost,1433", None)
        .await
        .map_err(|e| format!("Connection error: {e}"))
}

async fn query_handler() -> Result<String, String> {
    let mut client = create_client().await?;
    client
        .execute("SELECT @@VERSION".to_string(), None, None)
        .await
        .map_err(|e| format!("Execute error: {e}"))?;

    let mut result = String::new();
    if let Some(rs) = client.get_current_resultset() {
        while let Some(row) = rs.next_row().await.map_err(|e| format!("Row error: {e}"))? {
            result.push_str(&format!("{:?}\n", row));
        }
    }
    client
        .close_connection()
        .await
        .map_err(|e| format!("Close error: {e}"))?;
    Ok(if result.is_empty() {
        "No rows".to_string()
    } else {
        result
    })
}

async fn heavy_handler() -> Result<String, String> {
    let mut client = create_client().await?;
    client
        .execute(
            "SELECT TOP 1000 o.name, c.name FROM sys.objects o CROSS JOIN sys.columns c"
                .to_string(),
            None,
            None,
        )
        .await
        .map_err(|e| format!("Execute error: {e}"))?;

    let mut count = 0u64;
    if let Some(rs) = client.get_current_resultset() {
        while let Some(_row) = rs.next_row().await.map_err(|e| format!("Row error: {e}"))? {
            count += 1;
        }
    }
    client
        .close_connection()
        .await
        .map_err(|e| format!("Close error: {e}"))?;
    Ok(format!("Fetched {count} rows"))
}

async fn health_handler() -> &'static str {
    "ok"
}

async fn metrics_handler(
    registry: axum::extract::State<Arc<prometheus::Registry>>,
) -> Result<String, String> {
    let encoder = TextEncoder::new();
    let metric_families = registry.gather();
    let mut buffer = Vec::new();
    encoder
        .encode(&metric_families, &mut buffer)
        .map_err(|e| format!("Encode error: {e}"))?;
    String::from_utf8(buffer).map_err(|e| format!("UTF8 error: {e}"))
}

// bring traits into scope
use mssql_tds::connection::tds_client::{ResultSet, ResultSetClient};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    // Setup prometheus exporter for local /metrics
    let registry = prometheus::Registry::new();
    let prom_exporter = opentelemetry_prometheus::exporter()
        .with_registry(registry.clone())
        .build()
        .expect("Failed to build prometheus exporter");

    // Build OTLP exporter
    let otlp_exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .build()
        .expect("Failed to build OTLP exporter");

    let reader = opentelemetry_sdk::metrics::PeriodicReader::builder(otlp_exporter)
        .build();

    // Build meter provider with both readers
    let meter_provider = SdkMeterProvider::builder()
        .with_reader(prom_exporter)
        .with_reader(reader)
        .build();

    opentelemetry::global::set_meter_provider(meter_provider);

    // Activate mssql-tds runtime instrumentation now that a real provider is set.
    mssql_tds::io::packet_reader::otel_metrics::enable();

    let registry = Arc::new(registry);

    let app = Router::new()
        .route("/query", get(query_handler))
        .route("/heavy", get(heavy_handler))
        .route("/health", get(health_handler))
        .route("/metrics", get(metrics_handler))
        .with_state(registry);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3002").await.unwrap();
    tracing::info!("Listening on http://0.0.0.0:3002");
    axum::serve(listener, app).await.unwrap();
}
