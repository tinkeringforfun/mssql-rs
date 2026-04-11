//! Integration tests for the Linux LocalDB feature.
//!
//! These tests require root privileges to actually run (Linux namespace creation)
//! and will download ~700MB of SQL Server image data. They are `#[ignore]`d by
//! default — run with:
//!
//! ```bash
//! sudo cargo test -p mssql-tds --features localdb -- --ignored
//! ```

#[cfg(feature = "localdb")]
mod localdb_tests {
    use std::sync::Arc;

    /// Test that `get_or_start` returns a running instance descriptor.
    #[tokio::test]
    #[ignore = "Needs root + downloads ~700MB SQL Server image"]
    async fn get_or_start_returns_instance() {
        let instance = mssql_container::get_or_start()
            .await
            .expect("should start container");
        assert_eq!(instance.host, "127.0.0.1");
        assert!(instance.port > 0);
        assert!(!instance.sa_password.is_empty());
    }

    /// Test that calling `get_or_start` twice returns the same instance (singleton).
    #[tokio::test]
    #[ignore = "Needs root + downloads ~700MB SQL Server image"]
    async fn get_or_start_is_singleton() {
        let a = mssql_container::get_or_start()
            .await
            .expect("first call");
        let b = mssql_container::get_or_start()
            .await
            .expect("second call");
        assert_eq!(a.port, b.port);
        assert_eq!(a.sa_password, b.sa_password);
    }

    // NOTE: The tests below require mssql-tds to compile fully (it currently
    // has pre-existing errors in other modules). They are provided as a
    // template for when the crate compiles end-to-end.

    /*
    /// Test connecting via mssql-tds with server="(localdb)".
    #[tokio::test]
    #[ignore = "Needs root + downloads ~700MB SQL Server image"]
    async fn connect_via_localdb_hostname() {
        use mssql_tds::connection::client_context::ClientContext;
        use mssql_tds::connection_provider::tds_connection_provider::TdsConnectionProvider;

        let provider = TdsConnectionProvider::new();
        let context = ClientContext::default();
        let client = provider
            .create_client(context, "(localdb)", None)
            .await
            .expect("should connect via (localdb)");
        drop(client);
    }

    /// Test running a simple query through localdb.
    #[tokio::test]
    #[ignore = "Needs root + downloads ~700MB SQL Server image"]
    async fn simple_query_via_localdb() {
        use mssql_tds::connection::client_context::ClientContext;
        use mssql_tds::connection_provider::tds_connection_provider::TdsConnectionProvider;

        let provider = TdsConnectionProvider::new();
        let context = ClientContext::default();
        let mut client = provider
            .create_client(context, "makeitwork", None)
            .await
            .expect("should connect via makeitwork");
        // SELECT @@VERSION would go here once the query API is accessible
        drop(client);
    }

    /// Test that multiple concurrent connections share the same container.
    #[tokio::test]
    #[ignore = "Needs root + downloads ~700MB SQL Server image"]
    async fn concurrent_connections_share_container() {
        let handles: Vec<_> = (0..5)
            .map(|_| {
                tokio::spawn(async {
                    mssql_container::get_or_start().await.unwrap()
                })
            })
            .collect();
        let mut ports = Vec::new();
        for h in handles {
            let instance = h.await.unwrap();
            ports.push(instance.port);
        }
        // All should have the same port (same container).
        assert!(ports.windows(2).all(|w| w[0] == w[1]));
    }
    */
}
