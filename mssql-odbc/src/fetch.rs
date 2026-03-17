use crate::handle::*;
use crate::types::*;
use mssql_tds::connection::tds_client::ResultSet as TdsResultSet;
use std::ptr;

pub fn fetch(stmt: &mut Statement) -> SQLRETURN {
    if !stmt.executed {
        return SQL_ERROR;
    }

    // Reset read offsets on each new row
    stmt.read_offsets.clear();

    if stmt.streaming {
        // If prefetch buffer is empty and no terminal state, fill it
        if stmt.prefetch_buffer.is_empty() && stmt.prefetch_done.is_none() {
            let conn = unsafe { &mut *stmt.conn };
            let (client, rt) = match (conn.client.as_mut(), conn.runtime.as_ref()) {
                (Some(c), Some(r)) => (c, r),
                _ => return SQL_ERROR,
            };

            let prefetch_buffer = &mut stmt.prefetch_buffer;

            let terminal = rt.block_on(async {
                let mut writer = BulkRowWriter {
                    rows: prefetch_buffer,
                    current_row: Vec::new(),
                };
                match client.drain_all_rows_into(&mut writer).await {
                    Ok(_count) => Some(PrefetchTerminal::Done),
                    Err(e) => Some(PrefetchTerminal::Error(e.to_string())),
                }
            });

            stmt.prefetch_done = terminal;
        }

        // Pop from buffer
        match stmt.prefetch_buffer.pop_front() {
            Some(row) => {
                stmt.rows.clear();
                stmt.rows.push(row);
                stmt.row_index = 0;
                SQL_SUCCESS
            }
            None => {
                // Buffer empty — handle terminal state
                match stmt.prefetch_done.take() {
                    Some(PrefetchTerminal::Done) => {
                        stmt.streaming = false;
                        SQL_NO_DATA
                    }
                    Some(PrefetchTerminal::MoreResults) => {
                        stmt.streaming = false;
                        SQL_NO_DATA
                    }
                    Some(PrefetchTerminal::Error(msg)) => {
                        stmt.streaming = false;
                        stmt.diagnostics.push(DiagRecord {
                            state: "HY000".to_string(),
                            native_error: 0,
                            message: msg,
                        });
                        SQL_ERROR
                    }
                    None => SQL_NO_DATA,
                }
            }
        }
    } else {
        // Non-streaming mode (buffered rows)
        stmt.row_index += 1;
        if stmt.row_index as usize >= stmt.rows.len() {
            SQL_NO_DATA
        } else {
            SQL_SUCCESS
        }
    }
}

/// Helper: write a fixed-size numeric value to the target buffer
unsafe fn write_fixed<T: Copy>(
    target_value: SQLPOINTER,
    str_len_or_ind: *mut SQLLEN,
    val: T,
    read_offsets: &mut [usize],
    col_idx: usize,
) -> SQLRETURN {
    if !target_value.is_null() {
        *(target_value as *mut T) = val;
    }
    if !str_len_or_ind.is_null() {
        *str_len_or_ind = std::mem::size_of::<T>() as SQLLEN;
    }
    read_offsets[col_idx] = 0;
    SQL_SUCCESS
}

/// Helper: convert CellValue to i64 for numeric cross-type conversions
fn cell_to_i64(cell: &CellValue) -> i64 {
    match cell {
        CellValue::Bool(v) => *v as i64,
        CellValue::U8(v) => *v as i64,
        CellValue::I16(v) => *v as i64,
        CellValue::I32(v) => *v as i64,
        CellValue::I64(v) => *v,
        CellValue::F32(v) => *v as i64,
        CellValue::F64(v) => *v as i64,
        CellValue::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

fn cell_to_f64(cell: &CellValue) -> f64 {
    match cell {
        CellValue::Bool(v) => {
            if *v {
                1.0
            } else {
                0.0
            }
        }
        CellValue::U8(v) => *v as f64,
        CellValue::I16(v) => *v as f64,
        CellValue::I32(v) => *v as f64,
        CellValue::I64(v) => *v as f64,
        CellValue::F32(v) => *v as f64,
        CellValue::F64(v) => *v,
        CellValue::String(s) => s.parse().unwrap_or(0.0),
        CellValue::Decimal { value, scale, .. } => *value as f64 / 10f64.powi(*scale as i32),
        _ => 0.0,
    }
}

pub fn get_data(
    stmt: &mut Statement,
    col: SQLUSMALLINT,
    target_type: SQLSMALLINT,
    target_value: SQLPOINTER,
    buffer_length: SQLLEN,
    str_len_or_ind: *mut SQLLEN,
) -> SQLRETURN {
    if stmt.row_index < 0 || stmt.row_index as usize >= stmt.rows.len() {
        return SQL_ERROR;
    }
    let row = &stmt.rows[stmt.row_index as usize];
    let col_idx = (col as usize).wrapping_sub(1);
    if col_idx >= row.len() {
        return SQL_ERROR;
    }

    // Ensure read_offsets is large enough
    while stmt.read_offsets.len() <= col_idx {
        stmt.read_offsets.push(0);
    }

    let cell = &row[col_idx];

    // Handle NULL
    if matches!(cell, CellValue::Null) {
        if !str_len_or_ind.is_null() {
            unsafe {
                *str_len_or_ind = SQL_NULL_DATA;
            }
        }
        stmt.read_offsets[col_idx] = 0;
        return SQL_SUCCESS;
    }

    // Determine effective target type
    let eff_type = if target_type == SQL_C_DEFAULT {
        if col_idx < stmt.columns.len() {
            match stmt.columns[col_idx].sql_type {
                SQL_INTEGER => SQL_C_LONG,
                SQL_SMALLINT => SQL_C_SHORT,
                SQL_BIGINT => SQL_C_SBIGINT,
                SQL_DOUBLE | SQL_FLOAT => SQL_C_DOUBLE,
                SQL_REAL => SQL_C_FLOAT,
                SQL_BIT => SQL_C_BIT,
                SQL_TYPE_TIMESTAMP => SQL_C_TYPE_TIMESTAMP,
                SQL_TYPE_DATE => SQL_C_TYPE_DATE,
                SQL_TYPE_TIME => SQL_C_TYPE_TIME,
                SQL_BINARY | SQL_VARBINARY | SQL_LONGVARBINARY => SQL_C_BINARY,
                SQL_GUID => SQL_C_GUID,
                SQL_TINYINT => SQL_C_UTINYINT,
                _ => SQL_C_CHAR,
            }
        } else {
            SQL_C_CHAR
        }
    } else {
        target_type
    };

    match eff_type {
        SQL_C_LONG | SQL_C_SLONG => {
            let v: i32 = match cell {
                CellValue::I32(v) => *v,
                CellValue::Bool(v) => *v as i32,
                CellValue::U8(v) => *v as i32,
                CellValue::I16(v) => *v as i32,
                _ => cell_to_i64(cell) as i32,
            };
            unsafe {
                write_fixed(
                    target_value,
                    str_len_or_ind,
                    v,
                    &mut stmt.read_offsets,
                    col_idx,
                )
            }
        }
        SQL_C_SHORT => {
            let v: i16 = match cell {
                CellValue::I16(v) => *v,
                _ => cell_to_i64(cell) as i16,
            };
            unsafe {
                write_fixed(
                    target_value,
                    str_len_or_ind,
                    v,
                    &mut stmt.read_offsets,
                    col_idx,
                )
            }
        }
        SQL_C_SBIGINT => {
            let v: i64 = match cell {
                CellValue::I64(v) => *v,
                _ => cell_to_i64(cell),
            };
            unsafe {
                write_fixed(
                    target_value,
                    str_len_or_ind,
                    v,
                    &mut stmt.read_offsets,
                    col_idx,
                )
            }
        }
        SQL_C_DOUBLE => {
            let v: f64 = match cell {
                CellValue::F64(v) => *v,
                _ => cell_to_f64(cell),
            };
            unsafe {
                write_fixed(
                    target_value,
                    str_len_or_ind,
                    v,
                    &mut stmt.read_offsets,
                    col_idx,
                )
            }
        }
        SQL_C_FLOAT => {
            let v: f32 = match cell {
                CellValue::F32(v) => *v,
                _ => cell_to_f64(cell) as f32,
            };
            unsafe {
                write_fixed(
                    target_value,
                    str_len_or_ind,
                    v,
                    &mut stmt.read_offsets,
                    col_idx,
                )
            }
        }
        SQL_C_BIT => {
            let v: u8 = match cell {
                CellValue::Bool(b) => {
                    if *b { 1 } else { 0 }
                }
                CellValue::U8(v) => {
                    if *v != 0 { 1 } else { 0 }
                }
                CellValue::String(s) => {
                    if s == "0" || s.is_empty() { 0 } else { 1 }
                }
                _ => {
                    if cell_to_i64(cell) != 0 { 1 } else { 0 }
                }
            };
            unsafe {
                write_fixed(
                    target_value,
                    str_len_or_ind,
                    v,
                    &mut stmt.read_offsets,
                    col_idx,
                )
            }
        }
        SQL_C_UTINYINT | SQL_C_STINYINT => {
            let v: u8 = match cell {
                CellValue::U8(v) => *v,
                _ => cell_to_i64(cell) as u8,
            };
            unsafe {
                write_fixed(
                    target_value,
                    str_len_or_ind,
                    v,
                    &mut stmt.read_offsets,
                    col_idx,
                )
            }
        }
        SQL_C_WCHAR => {
            let utf16: std::borrow::Cow<[u16]> = match cell {
                CellValue::Utf16(u) => std::borrow::Cow::Borrowed(u.as_slice()),
                _ => {
                    let val = cell.to_string_repr().unwrap_or_default();
                    std::borrow::Cow::Owned(val.encode_utf16().collect())
                }
            };
            let total_bytes = (utf16.len() * 2) as SQLLEN;
            let offset = stmt.read_offsets[col_idx];
            let remaining_u16 = if offset < utf16.len() {
                &utf16[offset..]
            } else {
                if !str_len_or_ind.is_null() {
                    unsafe {
                        *str_len_or_ind = 0;
                    }
                }
                stmt.read_offsets[col_idx] = 0;
                return SQL_NO_DATA;
            };

            if offset == 0 {
                if !str_len_or_ind.is_null() {
                    unsafe {
                        *str_len_or_ind = total_bytes;
                    }
                }
            } else if !str_len_or_ind.is_null() {
                unsafe {
                    *str_len_or_ind = (remaining_u16.len() * 2) as SQLLEN;
                }
            }

            if !target_value.is_null() && buffer_length > 0 {
                let buf_u16_cap = (buffer_length as usize) / 2;
                let copy_count = std::cmp::min(remaining_u16.len(), buf_u16_cap.saturating_sub(1));
                let dest = target_value as *mut u16;
                unsafe {
                    ptr::copy_nonoverlapping(remaining_u16.as_ptr(), dest, copy_count);
                    *dest.add(copy_count) = 0;
                }
                stmt.read_offsets[col_idx] = offset + copy_count;
                if remaining_u16.len() > copy_count {
                    return SQL_SUCCESS_WITH_INFO;
                }
            }
            stmt.read_offsets[col_idx] = 0;
            SQL_SUCCESS
        }
        SQL_C_TYPE_TIMESTAMP => {
            let ts = match cell {
                CellValue::DateTime { micros } => {
                    let (year, month, day, h, mi, sec, millis) = micros_to_timestamp_parts(*micros);
                    SqlTimestampStruct {
                        year: year as i16,
                        month: month as u16,
                        day: day as u16,
                        hour: h as u16,
                        minute: mi as u16,
                        second: sec as u16,
                        fraction: millis * 1_000_000,
                    }
                }
                CellValue::DateTimeOffset { micros, .. } => {
                    let (year, month, day, h, mi, sec, millis) = micros_to_timestamp_parts(*micros);
                    SqlTimestampStruct {
                        year: year as i16,
                        month: month as u16,
                        day: day as u16,
                        hour: h as u16,
                        minute: mi as u16,
                        second: sec as u16,
                        fraction: millis * 1_000_000,
                    }
                }
                _ => {
                    let s = cell.to_string_repr().unwrap_or_default();
                    parse_timestamp(&s)
                }
            };
            if !target_value.is_null() {
                unsafe {
                    *(target_value as *mut SqlTimestampStruct) = ts;
                }
            }
            if !str_len_or_ind.is_null() {
                unsafe {
                    *str_len_or_ind = std::mem::size_of::<SqlTimestampStruct>() as SQLLEN;
                }
            }
            stmt.read_offsets[col_idx] = 0;
            SQL_SUCCESS
        }
        SQL_C_TYPE_DATE => {
            let ts = match cell {
                CellValue::DateTime { micros } | CellValue::DateTimeOffset { micros, .. } => {
                    let (year, month, day, ..) = micros_to_timestamp_parts(*micros);
                    SqlDateStruct {
                        year: year as i16,
                        month: month as u16,
                        day: day as u16,
                    }
                }
                CellValue::Date { .. } => {
                    let s = cell.to_string_repr().unwrap_or_default();
                    let ts = parse_timestamp(&s);
                    SqlDateStruct {
                        year: ts.year,
                        month: ts.month,
                        day: ts.day,
                    }
                }
                _ => {
                    let s = cell.to_string_repr().unwrap_or_default();
                    let ts = parse_timestamp(&s);
                    SqlDateStruct {
                        year: ts.year,
                        month: ts.month,
                        day: ts.day,
                    }
                }
            };
            if !target_value.is_null() {
                unsafe {
                    *(target_value as *mut SqlDateStruct) = ts;
                }
            }
            if !str_len_or_ind.is_null() {
                unsafe {
                    *str_len_or_ind = std::mem::size_of::<SqlDateStruct>() as SQLLEN;
                }
            }
            stmt.read_offsets[col_idx] = 0;
            SQL_SUCCESS
        }
        SQL_C_TYPE_TIME => {
            let ts = match cell {
                CellValue::Time { nanos } => {
                    let total_secs = (*nanos / 1_000_000_000) as u32;
                    SqlTimeStruct {
                        hour: (total_secs / 3600) as u16,
                        minute: ((total_secs % 3600) / 60) as u16,
                        second: (total_secs % 60) as u16,
                    }
                }
                CellValue::DateTime { micros } | CellValue::DateTimeOffset { micros, .. } => {
                    let (_, _, _, h, mi, sec, _) = micros_to_timestamp_parts(*micros);
                    SqlTimeStruct {
                        hour: h as u16,
                        minute: mi as u16,
                        second: sec as u16,
                    }
                }
                _ => {
                    let s = cell.to_string_repr().unwrap_or_default();
                    let ts = parse_timestamp(&s);
                    SqlTimeStruct {
                        hour: ts.hour,
                        minute: ts.minute,
                        second: ts.second,
                    }
                }
            };
            if !target_value.is_null() {
                unsafe {
                    *(target_value as *mut SqlTimeStruct) = ts;
                }
            }
            if !str_len_or_ind.is_null() {
                unsafe {
                    *str_len_or_ind = std::mem::size_of::<SqlTimeStruct>() as SQLLEN;
                }
            }
            stmt.read_offsets[col_idx] = 0;
            SQL_SUCCESS
        }
        SQL_C_BINARY => {
            let bytes: Vec<u8> = match cell {
                CellValue::Bytes(b) => b.clone(),
                CellValue::Guid(g) => g.to_vec(),
                _ => {
                    let s = cell.to_string_repr().unwrap_or_default();
                    if s.chars().all(|c| c.is_ascii_hexdigit()) && s.len() % 2 == 0 {
                        hex_decode(&s)
                    } else {
                        s.into_bytes()
                    }
                }
            };
            let offset = stmt.read_offsets[col_idx];
            let remaining = if offset < bytes.len() {
                &bytes[offset..]
            } else {
                if !str_len_or_ind.is_null() {
                    unsafe {
                        *str_len_or_ind = 0;
                    }
                }
                stmt.read_offsets[col_idx] = 0;
                return SQL_NO_DATA;
            };

            let remaining_len = remaining.len() as SQLLEN;
            if offset == 0 {
                if !str_len_or_ind.is_null() {
                    unsafe {
                        *str_len_or_ind = bytes.len() as SQLLEN;
                    }
                }
            } else if !str_len_or_ind.is_null() {
                unsafe {
                    *str_len_or_ind = remaining_len;
                }
            }

            if !target_value.is_null() && buffer_length > 0 {
                let copy_len = std::cmp::min(remaining_len, buffer_length) as usize;
                unsafe {
                    ptr::copy_nonoverlapping(remaining.as_ptr(), target_value as *mut u8, copy_len);
                }
                stmt.read_offsets[col_idx] = offset + copy_len;
                if remaining.len() > copy_len {
                    return SQL_SUCCESS_WITH_INFO;
                }
            }
            stmt.read_offsets[col_idx] = 0;
            SQL_SUCCESS
        }
        SQL_C_GUID => {
            let guid = match cell {
                CellValue::Guid(bytes) => SqlGuid {
                    data1: u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
                    data2: u16::from_be_bytes([bytes[4], bytes[5]]),
                    data3: u16::from_be_bytes([bytes[6], bytes[7]]),
                    data4: [
                        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                        bytes[15],
                    ],
                },
                _ => {
                    let s = cell.to_string_repr().unwrap_or_default();
                    parse_guid(&s)
                }
            };
            if !target_value.is_null() {
                unsafe {
                    *(target_value as *mut SqlGuid) = guid;
                }
            }
            if !str_len_or_ind.is_null() {
                unsafe {
                    *str_len_or_ind = 16;
                }
            }
            stmt.read_offsets[col_idx] = 0;
            SQL_SUCCESS
        }
        _ => {
            // SQL_C_CHAR or unknown: return as ANSI string with chunked read support
            let val = cell.to_string_repr().unwrap_or_default();
            let bytes = val.as_bytes();
            let offset = stmt.read_offsets[col_idx];

            let remaining = if offset < bytes.len() {
                &bytes[offset..]
            } else if offset > 0 {
                if !str_len_or_ind.is_null() {
                    unsafe {
                        *str_len_or_ind = 0;
                    }
                }
                stmt.read_offsets[col_idx] = 0;
                return SQL_NO_DATA;
            } else {
                bytes
            };

            let remaining_len = remaining.len() as SQLLEN;

            if offset == 0 {
                if !str_len_or_ind.is_null() {
                    unsafe {
                        *str_len_or_ind = bytes.len() as SQLLEN;
                    }
                }
            } else if !str_len_or_ind.is_null() {
                unsafe {
                    *str_len_or_ind = remaining_len;
                }
            }

            if !target_value.is_null() && buffer_length > 0 {
                let copy_len = std::cmp::min(remaining_len, buffer_length - 1) as usize;
                unsafe {
                    ptr::copy_nonoverlapping(remaining.as_ptr(), target_value as *mut u8, copy_len);
                    *((target_value as *mut u8).add(copy_len)) = 0;
                }
                stmt.read_offsets[col_idx] = offset + copy_len;
                if remaining.len() > copy_len {
                    return SQL_SUCCESS_WITH_INFO;
                }
            }
            stmt.read_offsets[col_idx] = 0;
            SQL_SUCCESS
        }
    }
}

pub fn num_result_cols(stmt: &Statement) -> SQLSMALLINT {
    stmt.columns.len() as SQLSMALLINT
}

pub fn describe_col(
    stmt: &Statement,
    col_number: SQLUSMALLINT,
    col_name: *mut SQLCHAR,
    buffer_length: SQLSMALLINT,
    name_length: *mut SQLSMALLINT,
    data_type: *mut SQLSMALLINT,
    column_size: *mut SQLULEN,
    decimal_digits: *mut SQLSMALLINT,
    nullable: *mut SQLSMALLINT,
) -> SQLRETURN {
    let idx = (col_number as usize).wrapping_sub(1);
    if idx >= stmt.columns.len() {
        return SQL_ERROR;
    }
    let col = &stmt.columns[idx];

    if !col_name.is_null() && buffer_length > 0 {
        let name_bytes = col.name.as_bytes();
        let copy_len = std::cmp::min(name_bytes.len(), (buffer_length as usize).saturating_sub(1));
        unsafe {
            ptr::copy_nonoverlapping(name_bytes.as_ptr(), col_name, copy_len);
            *col_name.add(copy_len) = 0;
        }
    }
    if !name_length.is_null() {
        unsafe {
            *name_length = col.name.len() as SQLSMALLINT;
        }
    }
    if !data_type.is_null() {
        unsafe {
            *data_type = col.sql_type;
        }
    }
    if !column_size.is_null() {
        unsafe {
            *column_size = col.size;
        }
    }
    if !decimal_digits.is_null() {
        unsafe {
            *decimal_digits = col.decimal_digits;
        }
    }
    if !nullable.is_null() {
        unsafe {
            *nullable = col.nullable;
        }
    }
    SQL_SUCCESS
}

fn parse_timestamp(s: &str) -> SqlTimestampStruct {
    let mut ts = SqlTimestampStruct::default();
    let parts: Vec<&str> = s.splitn(2, [' ', 'T']).collect();
    if let Some(date_part) = parts.first() {
        let d: Vec<&str> = date_part.split('-').collect();
        if d.len() >= 3 {
            ts.year = d[0].parse().unwrap_or(0);
            ts.month = d[1].parse().unwrap_or(0);
            ts.day = d[2].parse().unwrap_or(0);
        }
    }
    if let Some(time_part) = parts.get(1) {
        let time_str = time_part.split(['+', '-']).next().unwrap_or(time_part);
        let t: Vec<&str> = time_str.split(':').collect();
        if t.len() >= 3 {
            ts.hour = t[0].parse().unwrap_or(0);
            ts.minute = t[1].parse().unwrap_or(0);
            let sec_parts: Vec<&str> = t[2].split('.').collect();
            ts.second = sec_parts[0].parse().unwrap_or(0);
            if sec_parts.len() > 1 {
                let frac_str = sec_parts[1];
                let padded = format!("{:0<9}", frac_str);
                ts.fraction = padded[..9].parse().unwrap_or(0);
            }
        }
    }
    ts
}

fn hex_decode(s: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(s.len() / 2);
    let mut chars = s.chars();
    while let (Some(a), Some(b)) = (chars.next(), chars.next()) {
        let byte = u8::from_str_radix(&format!("{}{}", a, b), 16).unwrap_or(0);
        bytes.push(byte);
    }
    bytes
}

fn parse_guid(s: &str) -> SqlGuid {
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let bytes = hex_decode(&hex);
    if bytes.len() >= 16 {
        SqlGuid {
            data1: u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            data2: u16::from_be_bytes([bytes[4], bytes[5]]),
            data3: u16::from_be_bytes([bytes[6], bytes[7]]),
            data4: [
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ],
        }
    } else {
        SqlGuid {
            data1: 0,
            data2: 0,
            data3: 0,
            data4: [0; 8],
        }
    }
}

// ── Bound column fill (for SQLBindCol + columnar/array fetch) ───────

fn write_c_char(data_ptr: *mut u8, buf_len: SQLLEN, ind_ptr: *mut SQLLEN, bytes: &[u8]) {
    let max = if buf_len > 0 { (buf_len - 1) as usize } else { 0 };
    let copy_len = bytes.len().min(max);
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), data_ptr, copy_len);
        *data_ptr.add(copy_len) = 0;
    }
    if !ind_ptr.is_null() {
        unsafe { *ind_ptr = bytes.len() as SQLLEN; }
    }
}

fn write_c_wchar(data_ptr: *mut u8, buf_len: SQLLEN, ind_ptr: *mut SQLLEN, s: &str) {
    let utf16: Vec<u16> = s.encode_utf16().collect();
    let byte_len = utf16.len() * 2;
    let max_bytes = if buf_len > 1 { (buf_len - 2) as usize } else { 0 };
    let copy_bytes = byte_len.min(max_bytes);
    let copy_u16 = copy_bytes / 2;
    unsafe {
        ptr::copy_nonoverlapping(utf16.as_ptr() as *const u8, data_ptr, copy_u16 * 2);
        *(data_ptr.add(copy_u16 * 2) as *mut u16) = 0;
    }
    if !ind_ptr.is_null() {
        unsafe { *ind_ptr = byte_len as SQLLEN; }
    }
}

pub fn fill_bound_cols(stmt: &mut Statement) {
    fill_bound_cols_at(stmt, 0);
}

pub fn fill_bound_cols_at(stmt: &mut Statement, row_offset: usize) {
    let idx = stmt.row_index as usize;
    let row = match stmt.rows.get(idx) {
        Some(r) => r,
        None => return,
    };

    for col_idx in 0..row.len() {
        let bind_idx = col_idx + 1; // bound_cols is 1-based
        let binding = match stmt.bound_cols.get(bind_idx) {
            Some(Some(b)) => b,
            _ => continue,
        };

        let cell = &row[col_idx];
        let target_type = binding.target_type;
        let buf_len = binding.buffer_length;

        let element_size = if buf_len > 0 { buf_len as usize } else {
            match target_type {
                SQL_C_SLONG | SQL_C_LONG => 4,
                SQL_C_SBIGINT => 8,
                SQL_C_DOUBLE => 8,
                SQL_C_FLOAT => 4,
                SQL_C_SHORT => 2,
                SQL_C_STINYINT | SQL_C_UTINYINT | SQL_C_BIT => 1,
                SQL_C_TYPE_TIMESTAMP => std::mem::size_of::<SqlTimestampStruct>(),
                _ => buf_len.max(1) as usize,
            }
        };
        let data_ptr = unsafe { (binding.target_value as *mut u8).add(row_offset * element_size) };
        let ind_ptr = if binding.str_len_or_ind.is_null() {
            ptr::null_mut()
        } else {
            unsafe { binding.str_len_or_ind.add(row_offset) }
        };

        match cell {
            CellValue::Null => {
                if !ind_ptr.is_null() {
                    unsafe { *ind_ptr = SQL_NULL_DATA as SQLLEN; }
                }
            }
            CellValue::I32(v) => {
                match target_type {
                    SQL_C_SLONG | SQL_C_LONG => unsafe { *(data_ptr as *mut i32) = *v; },
                    SQL_C_SBIGINT => unsafe { *(data_ptr as *mut i64) = *v as i64; },
                    SQL_C_DOUBLE => unsafe { *(data_ptr as *mut f64) = *v as f64; },
                    SQL_C_CHAR => { write_c_char(data_ptr, buf_len, ind_ptr, v.to_string().as_bytes()); continue; }
                    SQL_C_WCHAR => { write_c_wchar(data_ptr, buf_len, ind_ptr, &v.to_string()); continue; }
                    _ => unsafe { *(data_ptr as *mut i32) = *v; },
                }
                if !ind_ptr.is_null() { unsafe { *ind_ptr = element_size as SQLLEN; } }
            }
            CellValue::I64(v) => {
                match target_type {
                    SQL_C_SBIGINT => unsafe { *(data_ptr as *mut i64) = *v; },
                    SQL_C_SLONG | SQL_C_LONG => unsafe { *(data_ptr as *mut i32) = *v as i32; },
                    SQL_C_DOUBLE => unsafe { *(data_ptr as *mut f64) = *v as f64; },
                    SQL_C_CHAR => { write_c_char(data_ptr, buf_len, ind_ptr, v.to_string().as_bytes()); continue; }
                    SQL_C_WCHAR => { write_c_wchar(data_ptr, buf_len, ind_ptr, &v.to_string()); continue; }
                    _ => unsafe { *(data_ptr as *mut i64) = *v; },
                }
                if !ind_ptr.is_null() { unsafe { *ind_ptr = element_size as SQLLEN; } }
            }
            CellValue::I16(v) => {
                match target_type {
                    SQL_C_SHORT => unsafe { *(data_ptr as *mut i16) = *v; },
                    SQL_C_SLONG | SQL_C_LONG => unsafe { *(data_ptr as *mut i32) = *v as i32; },
                    SQL_C_SBIGINT => unsafe { *(data_ptr as *mut i64) = *v as i64; },
                    SQL_C_DOUBLE => unsafe { *(data_ptr as *mut f64) = *v as f64; },
                    _ => unsafe { *(data_ptr as *mut i16) = *v; },
                }
                if !ind_ptr.is_null() { unsafe { *ind_ptr = element_size as SQLLEN; } }
            }
            CellValue::U8(v) => {
                unsafe { *data_ptr = *v; }
                if !ind_ptr.is_null() { unsafe { *ind_ptr = 1; } }
            }
            CellValue::F64(v) => {
                match target_type {
                    SQL_C_DOUBLE => unsafe { *(data_ptr as *mut f64) = *v; },
                    SQL_C_FLOAT => unsafe { *(data_ptr as *mut f32) = *v as f32; },
                    SQL_C_CHAR => { write_c_char(data_ptr, buf_len, ind_ptr, v.to_string().as_bytes()); continue; }
                    SQL_C_WCHAR => { write_c_wchar(data_ptr, buf_len, ind_ptr, &v.to_string()); continue; }
                    _ => unsafe { *(data_ptr as *mut f64) = *v; },
                }
                if !ind_ptr.is_null() { unsafe { *ind_ptr = element_size as SQLLEN; } }
            }
            CellValue::F32(v) => {
                match target_type {
                    SQL_C_FLOAT => unsafe { *(data_ptr as *mut f32) = *v; },
                    SQL_C_DOUBLE => unsafe { *(data_ptr as *mut f64) = *v as f64; },
                    _ => unsafe { *(data_ptr as *mut f32) = *v; },
                }
                if !ind_ptr.is_null() { unsafe { *ind_ptr = element_size as SQLLEN; } }
            }
            CellValue::String(s) => {
                match target_type {
                    SQL_C_WCHAR => write_c_wchar(data_ptr, buf_len, ind_ptr, s),
                    _ => write_c_char(data_ptr, buf_len, ind_ptr, s.as_bytes()),
                }
            }
            CellValue::Utf16(u) => {
                match target_type {
                    SQL_C_WCHAR => {
                        let byte_len = u.len() * 2;
                        let max_bytes = if buf_len > 1 { (buf_len - 2) as usize } else { 0 };
                        let copy_bytes = byte_len.min(max_bytes);
                        let copy_u16 = copy_bytes / 2;
                        unsafe {
                            ptr::copy_nonoverlapping(u.as_ptr() as *const u8, data_ptr, copy_u16 * 2);
                            *(data_ptr.add(copy_u16 * 2) as *mut u16) = 0;
                        }
                        if !ind_ptr.is_null() { unsafe { *ind_ptr = byte_len as SQLLEN; } }
                    }
                    _ => {
                        let s = String::from_utf16_lossy(u);
                        write_c_char(data_ptr, buf_len, ind_ptr, s.as_bytes());
                    }
                }
            }
            CellValue::Bool(v) => {
                unsafe { *data_ptr = if *v { 1 } else { 0 }; }
                if !ind_ptr.is_null() { unsafe { *ind_ptr = 1; } }
            }
            CellValue::DateTime { micros } | CellValue::DateTimeOffset { micros, .. } => {
                match target_type {
                    SQL_C_TYPE_TIMESTAMP => {
                        let (year, month, day, h, mi, sec, millis) = micros_to_timestamp_parts(*micros);
                        let ts = SqlTimestampStruct {
                            year: year as i16,
                            month: month as u16,
                            day: day as u16,
                            hour: h as u16,
                            minute: mi as u16,
                            second: sec as u16,
                            fraction: millis * 1_000_000,
                        };
                        unsafe { *(data_ptr as *mut SqlTimestampStruct) = ts; }
                        if !ind_ptr.is_null() {
                            unsafe { *ind_ptr = std::mem::size_of::<SqlTimestampStruct>() as SQLLEN; }
                        }
                    }
                    SQL_C_CHAR => {
                        let (year, month, day, h, mi, sec, millis) = micros_to_timestamp_parts(*micros);
                        let s = format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}", year, month, day, h, mi, sec, millis);
                        write_c_char(data_ptr, buf_len, ind_ptr, s.as_bytes());
                    }
                    SQL_C_WCHAR => {
                        let (year, month, day, h, mi, sec, millis) = micros_to_timestamp_parts(*micros);
                        let s = format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}", year, month, day, h, mi, sec, millis);
                        write_c_wchar(data_ptr, buf_len, ind_ptr, &s);
                    }
                    _ => {
                        let (year, month, day, h, mi, sec, millis) = micros_to_timestamp_parts(*micros);
                        let ts = SqlTimestampStruct {
                            year: year as i16,
                            month: month as u16,
                            day: day as u16,
                            hour: h as u16,
                            minute: mi as u16,
                            second: sec as u16,
                            fraction: millis * 1_000_000,
                        };
                        unsafe { *(data_ptr as *mut SqlTimestampStruct) = ts; }
                        if !ind_ptr.is_null() {
                            unsafe { *ind_ptr = std::mem::size_of::<SqlTimestampStruct>() as SQLLEN; }
                        }
                    }
                }
            }
            CellValue::Decimal { value, scale, .. } => {
                let fval = *value as f64 / 10f64.powi(*scale as i32);
                match target_type {
                    SQL_C_DOUBLE => unsafe { *(data_ptr as *mut f64) = fval; },
                    SQL_C_FLOAT => unsafe { *(data_ptr as *mut f32) = fval as f32; },
                    SQL_C_SBIGINT => unsafe { *(data_ptr as *mut i64) = fval as i64; },
                    SQL_C_SLONG | SQL_C_LONG => unsafe { *(data_ptr as *mut i32) = fval as i32; },
                    SQL_C_CHAR => {
                        let s = format_decimal(*value, *scale);
                        write_c_char(data_ptr, buf_len, ind_ptr, s.as_bytes());
                        continue;
                    }
                    SQL_C_WCHAR => {
                        let s = format_decimal(*value, *scale);
                        write_c_wchar(data_ptr, buf_len, ind_ptr, &s);
                        continue;
                    }
                    _ => unsafe { *(data_ptr as *mut f64) = fval; },
                }
                if !ind_ptr.is_null() { unsafe { *ind_ptr = element_size as SQLLEN; } }
            }
            CellValue::Bytes(b) => {
                let copy_len = b.len().min(buf_len.max(0) as usize);
                unsafe { ptr::copy_nonoverlapping(b.as_ptr(), data_ptr, copy_len); }
                if !ind_ptr.is_null() { unsafe { *ind_ptr = b.len() as SQLLEN; } }
            }
            CellValue::Guid(g) => {
                if target_type == SQL_C_GUID && element_size >= 16 {
                    unsafe { ptr::copy_nonoverlapping(g.as_ptr(), data_ptr, 16); }
                    if !ind_ptr.is_null() { unsafe { *ind_ptr = 16; } }
                } else {
                    let s = format!("{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
                        g[0],g[1],g[2],g[3],g[4],g[5],g[6],g[7],g[8],g[9],g[10],g[11],g[12],g[13],g[14],g[15]);
                    match target_type {
                        SQL_C_WCHAR => write_c_wchar(data_ptr, buf_len, ind_ptr, &s),
                        _ => write_c_char(data_ptr, buf_len, ind_ptr, s.as_bytes()),
                    }
                }
            }
            _ => {
                // Fallback: string representation
                let s = cell.to_string_repr().unwrap_or_default();
                match target_type {
                    SQL_C_WCHAR => write_c_wchar(data_ptr, buf_len, ind_ptr, &s),
                    _ => write_c_char(data_ptr, buf_len, ind_ptr, s.as_bytes()),
                }
            }
        }
    }
}

fn format_decimal(value: i128, scale: u8) -> String {
    if scale == 0 {
        return value.to_string();
    }
    let divisor = 10i128.pow(scale as u32);
    let integer_part = value / divisor;
    let frac_part = (value % divisor).unsigned_abs();
    format!("{}.{:0>width$}", integer_part, frac_part, width = scale as usize)
}
