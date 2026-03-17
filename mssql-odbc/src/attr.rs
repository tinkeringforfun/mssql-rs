use crate::execute;
use crate::types::*;
use std::ptr;

pub fn set_env_attr(
    env: &mut crate::handle::Environment,
    attribute: SQLINTEGER,
    value: SQLPOINTER,
    _string_length: SQLINTEGER,
) -> SQLRETURN {
    match attribute {
        SQL_ATTR_ODBC_VERSION => {
            env.odbc_version = value as SQLINTEGER;
            SQL_SUCCESS
        }
        _ => SQL_SUCCESS,
    }
}

pub fn set_connect_attr(
    conn: &mut crate::handle::Connection,
    attribute: SQLINTEGER,
    value: SQLPOINTER,
    _string_length: SQLINTEGER,
) -> SQLRETURN {
    match attribute {
        SQL_ATTR_AUTOCOMMIT => {
            let val = value as SQLULEN;
            let new_autocommit = val != 0;
            if conn.autocommit != new_autocommit {
                // If turning autocommit back on while in a transaction, commit
                if new_autocommit && conn.in_transaction {
                    let ret = execute::end_transaction(conn, true);
                    if ret != SQL_SUCCESS {
                        return SQL_ERROR;
                    }
                }
                conn.autocommit = new_autocommit;
            }
            SQL_SUCCESS
        }
        SQL_ATTR_LOGIN_TIMEOUT | SQL_ATTR_CONNECTION_TIMEOUT => SQL_SUCCESS,
        _ => SQL_SUCCESS,
    }
}

pub fn get_info(
    _conn: &crate::handle::Connection,
    info_type: SQLUSMALLINT,
    info_value: SQLPOINTER,
    buffer_length: SQLSMALLINT,
    string_length: *mut SQLSMALLINT,
) -> SQLRETURN {
    let write_str = |s: &str| -> SQLRETURN {
        let bytes = s.as_bytes();
        if !string_length.is_null() {
            unsafe {
                *string_length = bytes.len() as SQLSMALLINT;
            }
        }
        if !info_value.is_null() && buffer_length > 0 {
            let copy_len = std::cmp::min(bytes.len(), (buffer_length as usize).saturating_sub(1));
            unsafe {
                ptr::copy_nonoverlapping(bytes.as_ptr(), info_value as *mut u8, copy_len);
                *((info_value as *mut u8).add(copy_len)) = 0;
            }
        }
        SQL_SUCCESS
    };

    let write_u16 = |v: u16| -> SQLRETURN {
        if !info_value.is_null() {
            unsafe {
                *(info_value as *mut u16) = v;
            }
        }
        if !string_length.is_null() {
            unsafe {
                *string_length = 2;
            }
        }
        SQL_SUCCESS
    };

    let write_u32 = |v: u32| -> SQLRETURN {
        if !info_value.is_null() {
            unsafe {
                *(info_value as *mut u32) = v;
            }
        }
        if !string_length.is_null() {
            unsafe {
                *string_length = 4;
            }
        }
        SQL_SUCCESS
    };

    match info_type {
        SQL_DRIVER_NAME => write_str("libmssql_odbc.so"),
        SQL_DRIVER_VER => write_str("01.00.0000"),
        SQL_DBMS_NAME => write_str("Microsoft SQL Server"),
        SQL_DBMS_VER => write_str("16.00.0000"),
        SQL_SERVER_NAME => write_str(&_conn.server),
        SQL_DATABASE_NAME => write_str(&_conn.database),
        SQL_USER_NAME => write_str(&_conn.uid),
        SQL_DATA_SOURCE_NAME => write_str(""),
        SQL_SEARCH_PATTERN_ESCAPE => write_str("\\"),
        SQL_IDENTIFIER_QUOTE_CHAR => write_str("\""),
        SQL_CATALOG_NAME_SEPARATOR => write_str("."),
        SQL_CATALOG_TERM => write_str("catalog"),
        SQL_SCHEMA_TERM => write_str("schema"),
        SQL_TABLE_TERM => write_str("table"),
        SQL_NEED_LONG_DATA_LEN => write_str("N"),
        SQL_ACCESSIBLE_TABLES => write_str("Y"),
        SQL_ACCESSIBLE_PROCEDURES => write_str("Y"),
        SQL_MULT_RESULT_SETS => write_str("Y"),
        SQL_MULTIPLE_ACTIVE_TXN => write_str("Y"),
        SQL_DESCRIBE_PARAMETER => write_str("Y"),
        SQL_PROCEDURES => write_str("Y"),
        SQL_COLUMN_ALIAS => write_str("Y"),
        SQL_EXPRESSIONS_IN_ORDERBY => write_str("Y"),
        SQL_OUTER_JOINS => write_str("Y"),
        SQL_ORDER_BY_COLUMNS_IN_SELECT => write_str("Y"),
        SQL_SPECIAL_CHARACTERS => write_str("_@#$"),
        SQL_MAX_DRIVER_CONNECTIONS => write_u16(0),
        SQL_CURSOR_COMMIT_BEHAVIOR => write_u16(0),
        SQL_CURSOR_ROLLBACK_BEHAVIOR => write_u16(0),
        SQL_TXN_CAPABLE => write_u16(SQL_TC_ALL),
        SQL_CONCAT_NULL_BEHAVIOR => write_u16(0),
        SQL_CORRELATION_NAME => write_u16(2),
        SQL_GROUP_BY => write_u16(2),
        SQL_QUOTED_IDENTIFIER_CASE => write_u16(3),
        SQL_NON_NULLABLE_COLUMNS => write_u16(1),
        SQL_NULL_COLLATION => write_u16(0),
        SQL_MAX_COLUMNS_IN_GROUP_BY => write_u16(0),
        SQL_MAX_COLUMNS_IN_ORDER_BY => write_u16(0),
        SQL_MAX_COLUMNS_IN_SELECT => write_u16(0),
        SQL_MAX_CATALOG_NAME_LEN => write_u16(128),
        SQL_MAX_SCHEMA_NAME_LEN => write_u16(128),
        SQL_MAX_TABLE_NAME_LEN => write_u16(128),
        SQL_MAX_COLUMN_NAME_LEN => write_u16(128),
        SQL_MAX_IDENTIFIER_LEN => write_u16(128),
        SQL_GETDATA_EXTENSIONS => write_u32(SQL_GD_ANY_COLUMN | SQL_GD_ANY_ORDER),
        SQL_TXN_ISOLATION_OPTION => write_u32(0x0F),
        SQL_DEFAULT_TXN_ISOLATION => write_u32(2),
        SQL_SUBQUERIES => write_u32(0x1F),
        SQL_UNION => write_u32(3),
        _ => write_str(""),
    }
}

pub fn set_stmt_attr(
    stmt: &mut crate::handle::Statement,
    attribute: SQLINTEGER,
    value: SQLPOINTER,
    _string_length: SQLINTEGER,
) -> SQLRETURN {
    const SQL_ATTR_PARAMSET_SIZE: SQLINTEGER = 22;
    match attribute {
        SQL_ATTR_PARAMSET_SIZE => {
            let size = value as usize;
            if size > 1 {
                stmt.diagnostics.push(crate::handle::DiagRecord {
                    state: "HYC00".to_string(),
                    native_error: 0,
                    message: "Array parameter binding (paramset_size > 1) not supported"
                        .to_string(),
                });
                return SQL_ERROR;
            }
            stmt.paramset_size = 1;
            SQL_SUCCESS
        }
        SQL_ATTR_ROW_ARRAY_SIZE => {
            stmt.row_array_size = (value as usize).max(1);
            SQL_SUCCESS
        }
        SQL_ATTR_ROWS_FETCHED_PTR => {
            stmt.rows_fetched_ptr = value as *mut SQLULEN;
            SQL_SUCCESS
        }
        SQL_ATTR_ROW_STATUS_PTR => {
            stmt.row_status_ptr = value as *mut u16;
            SQL_SUCCESS
        }
        _ => SQL_SUCCESS,
    }
}

pub fn get_stmt_attr(
    stmt: &crate::handle::Statement,
    attribute: SQLINTEGER,
    value: SQLPOINTER,
    _buffer_length: SQLINTEGER,
    string_length: *mut SQLINTEGER,
) -> SQLRETURN {
    match attribute {
        SQL_ATTR_ROW_ARRAY_SIZE => {
            if !value.is_null() {
                unsafe { *(value as *mut SQLULEN) = stmt.row_array_size as SQLULEN; }
            }
            if !string_length.is_null() {
                unsafe { *string_length = std::mem::size_of::<SQLULEN>() as SQLINTEGER; }
            }
            SQL_SUCCESS
        }
        SQL_ATTR_ROWS_FETCHED_PTR => {
            if !value.is_null() {
                unsafe { *(value as *mut *mut SQLULEN) = stmt.rows_fetched_ptr; }
            }
            SQL_SUCCESS
        }
        SQL_ATTR_ROW_STATUS_PTR => {
            if !value.is_null() {
                unsafe { *(value as *mut *mut u16) = stmt.row_status_ptr; }
            }
            SQL_SUCCESS
        }
        _ => SQL_SUCCESS,
    }
}

pub fn get_info_w(
    conn: &crate::handle::Connection,
    info_type: SQLUSMALLINT,
    info_value: SQLPOINTER,
    buffer_length: SQLSMALLINT,
    string_length: *mut SQLSMALLINT,
) -> SQLRETURN {
    let write_str_w = |s: &str| -> SQLRETURN {
        let utf16: Vec<u16> = s.encode_utf16().collect();
        let byte_len = (utf16.len() * 2) as SQLSMALLINT;
        if !string_length.is_null() {
            unsafe {
                *string_length = byte_len;
            }
        }
        if !info_value.is_null() && buffer_length > 0 {
            let buf_cap = (buffer_length as usize) / 2;
            let copy_count = std::cmp::min(utf16.len(), buf_cap.saturating_sub(1));
            let dest = info_value as *mut u16;
            unsafe {
                ptr::copy_nonoverlapping(utf16.as_ptr(), dest, copy_count);
                *dest.add(copy_count) = 0;
            }
        }
        SQL_SUCCESS
    };

    let write_u16 = |v: u16| -> SQLRETURN {
        if !info_value.is_null() {
            unsafe {
                *(info_value as *mut u16) = v;
            }
        }
        if !string_length.is_null() {
            unsafe {
                *string_length = 2;
            }
        }
        SQL_SUCCESS
    };

    let write_u32 = |v: u32| -> SQLRETURN {
        if !info_value.is_null() {
            unsafe {
                *(info_value as *mut u32) = v;
            }
        }
        if !string_length.is_null() {
            unsafe {
                *string_length = 4;
            }
        }
        SQL_SUCCESS
    };

    match info_type {
        SQL_DRIVER_NAME => write_str_w("libmssql_odbc.so"),
        SQL_DRIVER_VER => write_str_w("01.00.0000"),
        SQL_DBMS_NAME => write_str_w("Microsoft SQL Server"),
        SQL_DBMS_VER => write_str_w("16.00.0000"),
        SQL_SERVER_NAME => write_str_w(&conn.server),
        SQL_DATABASE_NAME => write_str_w(&conn.database),
        SQL_USER_NAME => write_str_w(&conn.uid),
        SQL_DATA_SOURCE_NAME => write_str_w(""),
        SQL_SEARCH_PATTERN_ESCAPE => write_str_w("\\"),
        SQL_IDENTIFIER_QUOTE_CHAR => write_str_w("\""),
        SQL_CATALOG_NAME_SEPARATOR => write_str_w("."),
        SQL_CATALOG_TERM => write_str_w("catalog"),
        SQL_SCHEMA_TERM => write_str_w("schema"),
        SQL_TABLE_TERM => write_str_w("table"),
        SQL_NEED_LONG_DATA_LEN => write_str_w("N"),
        SQL_ACCESSIBLE_TABLES => write_str_w("Y"),
        SQL_ACCESSIBLE_PROCEDURES => write_str_w("Y"),
        SQL_MULT_RESULT_SETS => write_str_w("Y"),
        SQL_MULTIPLE_ACTIVE_TXN => write_str_w("Y"),
        SQL_DESCRIBE_PARAMETER => write_str_w("Y"),
        SQL_PROCEDURES => write_str_w("Y"),
        SQL_COLUMN_ALIAS => write_str_w("Y"),
        SQL_EXPRESSIONS_IN_ORDERBY => write_str_w("Y"),
        SQL_OUTER_JOINS => write_str_w("Y"),
        SQL_ORDER_BY_COLUMNS_IN_SELECT => write_str_w("Y"),
        SQL_SPECIAL_CHARACTERS => write_str_w("_@#$"),
        SQL_MAX_DRIVER_CONNECTIONS => write_u16(0),
        SQL_CURSOR_COMMIT_BEHAVIOR => write_u16(0),
        SQL_CURSOR_ROLLBACK_BEHAVIOR => write_u16(0),
        SQL_TXN_CAPABLE => write_u16(SQL_TC_ALL),
        SQL_CONCAT_NULL_BEHAVIOR => write_u16(0),
        SQL_CORRELATION_NAME => write_u16(2),
        SQL_GROUP_BY => write_u16(2),
        SQL_QUOTED_IDENTIFIER_CASE => write_u16(3),
        SQL_NON_NULLABLE_COLUMNS => write_u16(1),
        SQL_NULL_COLLATION => write_u16(0),
        SQL_MAX_COLUMNS_IN_GROUP_BY => write_u16(0),
        SQL_MAX_COLUMNS_IN_ORDER_BY => write_u16(0),
        SQL_MAX_COLUMNS_IN_SELECT => write_u16(0),
        SQL_MAX_CATALOG_NAME_LEN => write_u16(128),
        SQL_MAX_SCHEMA_NAME_LEN => write_u16(128),
        SQL_MAX_TABLE_NAME_LEN => write_u16(128),
        SQL_MAX_COLUMN_NAME_LEN => write_u16(128),
        SQL_MAX_IDENTIFIER_LEN => write_u16(128),
        SQL_GETDATA_EXTENSIONS => write_u32(SQL_GD_ANY_COLUMN | SQL_GD_ANY_ORDER),
        SQL_TXN_ISOLATION_OPTION => write_u32(0x0F),
        SQL_DEFAULT_TXN_ISOLATION => write_u32(2),
        SQL_SUBQUERIES => write_u32(0x1F),
        SQL_UNION => write_u32(3),
        _ => write_str_w(""),
    }
}
