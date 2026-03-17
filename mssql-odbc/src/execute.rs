use crate::handle::*;
use crate::types::*;
use mssql_tds::connection::tds_client::{ResultSet as TdsResultSet, ResultSetClient};
use mssql_tds::message::transaction_management::TransactionIsolationLevel;

pub fn exec_direct(stmt: &mut Statement, sql: &str) -> SQLRETURN {
    let conn = unsafe { &mut *stmt.conn };

    let (client, rt) = match (conn.client.as_mut(), conn.runtime.as_ref()) {
        (Some(c), Some(r)) => (c, r),
        _ => {
            stmt.diagnostics.push(DiagRecord {
                state: "08003".to_string(),
                native_error: 0,
                message: "Not connected".to_string(),
            });
            return SQL_ERROR;
        }
    };

    // If we were previously streaming, drain the old stream first
    if stmt.streaming {
        let _ = rt.block_on(client.close_query());
        stmt.streaming = false;
    }

    // If autocommit is OFF and we're not already in a transaction, start one
    if !conn.autocommit && !conn.in_transaction {
        let begin_result = rt.block_on(
            client.begin_transaction(TransactionIsolationLevel::ReadCommitted, None),
        );
        if let Err(e) = begin_result {
            stmt.diagnostics.push(DiagRecord {
                state: "HY000".to_string(),
                native_error: 0,
                message: format!("Failed to begin transaction: {}", e),
            });
            return SQL_ERROR;
        }
        conn.in_transaction = true;
    }

    let sql = sql.to_string();

    // Execute the query
    let exec_result = rt.block_on(client.execute(sql, None, None));

    match exec_result {
        Ok(()) => {
            // Check if we have metadata (i.e., result set)
            let metadata = client.get_metadata();
            if metadata.is_empty() {
                // No result set (DML statement)
                stmt.columns = Vec::new();
                stmt.rows = Vec::new();
                stmt.row_count = -1; // TODO: get actual rows affected
                stmt.row_index = -1;
                stmt.executed = true;
                stmt.streaming = false;
                stmt.read_offsets.clear();
                stmt.pending_result_sets.clear();
                stmt.current_row.clear();
                stmt.prefetch_buffer.clear();
                stmt.prefetch_done = None;
            } else {
                // Has result set — set up columns, enable streaming
                stmt.columns = metadata
                    .iter()
                    .map(|c| {
                        let (sql_type, size, decimal_digits, nullable) = sql_type_from_metadata(c);
                        ColumnDesc {
                            name: c.column_name.clone(),
                            sql_type,
                            size,
                            decimal_digits,
                            nullable,
                        }
                    })
                    .collect();
                stmt.rows = Vec::new();
                stmt.row_count = -1;
                stmt.row_index = -1;
                stmt.executed = true;
                stmt.streaming = true;
                stmt.read_offsets.clear();
                stmt.pending_result_sets.clear();
                stmt.current_row.clear();
                stmt.prefetch_buffer.clear();
                stmt.prefetch_done = None;
            }
            SQL_SUCCESS
        }
        Err(e) => {
            let (state, native) = map_sqlstate(&e.to_string());
            stmt.diagnostics.push(DiagRecord {
                state,
                native_error: native,
                message: e.to_string(),
            });
            SQL_ERROR
        }
    }
}

/// Parse SQL Server error number from error message and map to SQLSTATE
fn map_sqlstate(msg: &str) -> (String, i32) {
    let native = extract_error_number(msg);
    let state = match native {
        2627 | 2601 | 547 => "23000",
        208 => "42S02",
        156 | 102 => "42000",
        _ => "HY000",
    };
    (state.to_string(), native)
}

fn extract_error_number(msg: &str) -> i32 {
    // mssql-tds SqlServerError format: "Sql Error: {number}: Class ..."
    if let Some(idx) = msg.find("Sql Error: ") {
        let rest = &msg[idx + 11..];
        if let Some(end) = rest.find(':') {
            if let Ok(n) = rest[..end].trim().parse::<i32>() {
                return n;
            }
        }
    }
    if let Some(idx) = msg.find("code: ") {
        let rest = &msg[idx + 6..];
        if let Some(end) = rest.find(|c: char| !c.is_ascii_digit()) {
            if let Ok(n) = rest[..end].parse::<i32>() {
                return n;
            }
        } else if let Ok(n) = rest.parse::<i32>() {
            return n;
        }
    }
    if let Some(idx) = msg.find("number: ") {
        let rest = &msg[idx + 8..];
        if let Some(end) = rest.find(|c: char| !c.is_ascii_digit()) {
            if let Ok(n) = rest[..end].parse::<i32>() {
                return n;
            }
        } else if let Ok(n) = rest.parse::<i32>() {
            return n;
        }
    }
    0
}

/// Execute a commit or rollback via TdsClient
pub fn end_transaction(conn: &mut Connection, commit: bool) -> SQLRETURN {
    let (client, rt) = match (conn.client.as_mut(), conn.runtime.as_ref()) {
        (Some(c), Some(r)) => (c, r),
        _ => return SQL_ERROR,
    };

    let result = if commit {
        rt.block_on(client.commit_transaction(None, None))
    } else {
        rt.block_on(client.rollback_transaction(None, None))
    };

    conn.in_transaction = false;

    match result {
        Ok(()) => SQL_SUCCESS,
        Err(e) => {
            conn.diagnostics.push(DiagRecord {
                state: "HY000".to_string(),
                native_error: 0,
                message: e.to_string(),
            });
            SQL_ERROR
        }
    }
}

/// Drain remaining rows and move to next result set (for SQLMoreResults)
pub fn move_to_next_result_set(stmt: &mut Statement) -> SQLRETURN {
    let conn = unsafe { &mut *stmt.conn };
    let (client, rt) = match (conn.client.as_mut(), conn.runtime.as_ref()) {
        (Some(c), Some(r)) => (c, r),
        _ => return SQL_NO_DATA,
    };

    // Use ResultSetClient::move_to_next which drains current result set and advances
    let result = rt.block_on(client.move_to_next());

    match result {
        Ok(true) => {
            // New result set available
            let metadata = client.get_metadata();
            stmt.columns = metadata
                .iter()
                .map(|c| {
                    let (sql_type, size, decimal_digits, nullable) = sql_type_from_metadata(c);
                    ColumnDesc {
                        name: c.column_name.clone(),
                        sql_type,
                        size,
                        decimal_digits,
                        nullable,
                    }
                })
                .collect();
            stmt.rows.clear();
            stmt.row_index = -1;
            stmt.read_offsets.clear();
            stmt.row_count = -1;
            stmt.streaming = true;
            stmt.prefetch_buffer.clear();
            stmt.prefetch_done = None;
            SQL_SUCCESS
        }
        Ok(false) => {
            stmt.streaming = false;
            SQL_NO_DATA
        }
        Err(_) => {
            stmt.streaming = false;
            SQL_NO_DATA
        }
    }
}
