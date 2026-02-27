use crate::handle::*;
use crate::types::*;
use mssql_tds::connection::client_context::ClientContext;
use mssql_tds::connection::tds_client::TdsClient;
use mssql_tds::connection_provider::tds_connection_provider::TdsConnectionProvider;
use mssql_tds::core::EncryptionOptions;
use mssql_tds::core::EncryptionSetting;

pub fn parse_connection_string(conn_str: &str) -> (String, u16, String, String, String, bool) {
    let mut host = "localhost".to_string();
    let mut port: u16 = 1433;
    let mut database = "master".to_string();
    let mut uid = String::new();
    let mut pwd = String::new();
    let mut trust_cert = false;

    for part in conn_str.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(idx) = part.find('=') {
            let key = part[..idx].trim().to_lowercase();
            let val = part[idx + 1..].trim().to_string();
            match key.as_str() {
                "server" => {
                    if let Some(comma) = val.find(',') {
                        host = val[..comma].to_string();
                        if let Ok(p) = val[comma + 1..].trim().parse() {
                            port = p;
                        }
                    } else {
                        host = val;
                    }
                }
                "database" | "initial catalog" => database = val,
                "uid" | "user id" => uid = val,
                "pwd" | "password" => pwd = val,
                "trustservercertificate" => {
                    trust_cert = val.eq_ignore_ascii_case("yes")
                        || val == "1"
                        || val.eq_ignore_ascii_case("true")
                }
                _ => {}
            }
        }
    }
    (host, port, database, uid, pwd, trust_cert)
}

pub fn driver_connect(conn: &mut Connection, conn_str: &str) -> SQLRETURN {
    let (host, port, database, uid, pwd, trust_cert) = parse_connection_string(conn_str);
    conn.server = format!("{}:{}", host, port);
    conn.database = database.clone();
    conn.uid = uid.clone();
    conn.pwd = pwd.clone();

    // Build the datasource string for TdsConnectionProvider
    let datasource = format!("tcp:{},{}", host, port);

    // Build a ClientContext
    let mut context = ClientContext::default();
    context.user_name = uid;
    context.password = pwd;
    context.database = database;

    if trust_cert {
        context.encryption_options = EncryptionOptions {
            mode: EncryptionSetting::PreferOff,
            trust_server_certificate: true,
            host_name_in_cert: None,
            server_certificate: None,
        };
    } else {
        context.encryption_options = EncryptionOptions {
            mode: EncryptionSetting::PreferOff,
            trust_server_certificate: false,
            host_name_in_cert: None,
            server_certificate: None,
        };
    }

    // Create tokio runtime if we don't have one
    let rt = match &conn.runtime {
        Some(rt) => rt,
        None => {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => {
                    conn.diagnostics.push(DiagRecord {
                        state: "HY000".to_string(),
                        native_error: 0,
                        message: format!("Failed to create tokio runtime: {}", e),
                    });
                    return SQL_ERROR;
                }
            };
            conn.runtime = Some(rt);
            conn.runtime.as_ref().unwrap()
        }
    };

    let result = rt.block_on(async {
        let provider = TdsConnectionProvider::new();
        provider.create_client(context, &datasource, None).await
    });

    match result {
        Ok(client) => {
            conn.client = Some(client);
            conn.connected = true;
            SQL_SUCCESS
        }
        Err(e) => {
            conn.diagnostics.push(DiagRecord {
                state: "08001".to_string(),
                native_error: 0,
                message: e.to_string(),
            });
            SQL_ERROR
        }
    }
}

pub fn disconnect(conn: &mut Connection) -> SQLRETURN {
    // Close connection properly
    if let (Some(client), Some(rt)) = (conn.client.as_mut(), conn.runtime.as_ref()) {
        let _ = rt.block_on(client.close_connection());
    }
    conn.client = None;
    conn.connected = false;
    SQL_SUCCESS
}
