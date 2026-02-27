use std::collections::VecDeque;

use crate::types::*;
use mssql_tds::connection::tds_client::TdsClient;
use mssql_tds::datatypes::column_values::{
    SqlDate, SqlDateTime, SqlDateTime2, SqlDateTimeOffset, SqlMoney, SqlSmallDateTime,
    SqlSmallMoney, SqlTime,
};
use mssql_tds::datatypes::decoder::DecimalParts;
use mssql_tds::datatypes::row_writer::RowWriter;
use mssql_tds::datatypes::column_values::SqlXml;
use mssql_tds::datatypes::sql_json::SqlJson;
use mssql_tds::datatypes::sql_string::SqlString;
use mssql_tds::datatypes::sql_vector::SqlVector;
use mssql_tds::datatypes::sqldatatypes::TdsDataType;
use mssql_tds::query::metadata::ColumnMetadata;
use uuid::Uuid;

/// Typed cell value — avoids string round-tripping for native types
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum CellValue {
    Null,
    Bool(bool),
    U8(u8),
    I16(i16),
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    String(String),
    Bytes(Vec<u8>),
    Date {
        days: i32,
    },
    Time {
        nanos: i64,
    },
    DateTime {
        micros: i64,
    },
    DateTimeOffset {
        micros: i64,
        offset_min: i16,
    },
    Decimal {
        value: i128,
        precision: u8,
        scale: u8,
    },
    Guid([u8; 16]),
    /// Raw UTF-16 data — avoids UTF-16→UTF-8→UTF-16 round-trip for SQL_C_WCHAR
    Utf16(Vec<u16>),
}

impl CellValue {
    /// Convert any CellValue to its string representation (for SQL_C_CHAR / SQL_C_WCHAR cross-type)
    pub fn to_string_repr(&self) -> Option<String> {
        match self {
            CellValue::Null => None,
            CellValue::Bool(v) => Some(if *v { "1".to_string() } else { "0".to_string() }),
            CellValue::U8(v) => Some(v.to_string()),
            CellValue::I16(v) => Some(v.to_string()),
            CellValue::I32(v) => Some(v.to_string()),
            CellValue::I64(v) => Some(v.to_string()),
            CellValue::F32(v) => Some(v.to_string()),
            CellValue::F64(v) => Some(v.to_string()),
            CellValue::String(s) => Some(s.clone()),
            CellValue::Bytes(b) => Some(hex::encode(b)),
            CellValue::Date { days } => Some(format_date(*days)),
            CellValue::Time { nanos } => Some(format_time(*nanos)),
            CellValue::DateTime { micros } => Some(format_datetime(*micros)),
            CellValue::DateTimeOffset { micros, offset_min } => {
                let mut s = format_datetime(*micros);
                let sign = if *offset_min >= 0 { "+" } else { "-" };
                let abs = offset_min.unsigned_abs();
                s.push_str(&format!(" {}{:02}:{:02}", sign, abs / 60, abs % 60));
                Some(s)
            }
            CellValue::Decimal { value, scale, .. } => Some(format_decimal(*value, *scale)),
            CellValue::Guid(bytes) => Some(format_guid_str(bytes)),
            CellValue::Utf16(u) => Some(String::from_utf16_lossy(u)),
        }
    }
}

fn days_to_ymd(days: i32) -> (i32, u32, u32) {
    let d = days + 719468i32;
    let era = if d >= 0 { d } else { d - 146096 } / 146097;
    let doe = (d - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i32 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, day)
}

fn format_date(days: i32) -> String {
    let (y, m, d) = days_to_ymd(days);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn format_time(nanos: i64) -> String {
    let total_secs = (nanos / 1_000_000_000) as u32;
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    let frac = (nanos % 1_000_000_000) / 1_000_000;
    format!("{:02}:{:02}:{:02}.{:03}", h, m, s, frac)
}

pub fn micros_to_timestamp_parts(micros: i64) -> (i32, u32, u32, u32, u32, u32, u32) {
    let total_secs = micros.div_euclid(1_000_000);
    let remaining_micros = micros.rem_euclid(1_000_000) as u32;
    let time_of_day = total_secs.rem_euclid(86400) as u32;
    let h = time_of_day / 3600;
    let mi = (time_of_day % 3600) / 60;
    let sec = time_of_day % 60;
    let millis = remaining_micros / 1000;
    let days = total_secs.div_euclid(86400) as i32;
    let (year, month, day) = days_to_ymd(days);
    (year, month, day, h, mi, sec, millis)
}

fn format_datetime(micros: i64) -> String {
    let (year, m, d, h, mi, sec, millis) = micros_to_timestamp_parts(micros);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        year, m, d, h, mi, sec, millis
    )
}

fn format_decimal(value: i128, scale: u8) -> String {
    let negative = value < 0;
    let abs = value.unsigned_abs();
    let s = abs.to_string();
    let scale = scale as usize;
    let result = if scale == 0 {
        s
    } else if s.len() <= scale {
        format!("0.{}{}", "0".repeat(scale - s.len()), s)
    } else {
        let (int_part, frac_part) = s.split_at(s.len() - scale);
        format!("{}.{}", int_part, frac_part)
    };
    if negative {
        format!("-{}", result)
    } else {
        result
    }
}

fn format_guid_str(bytes: &[u8; 16]) -> String {
    format!(
        "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        u16::from_be_bytes([bytes[4], bytes[5]]),
        u16::from_be_bytes([bytes[6], bytes[7]]),
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

/// Diagnostic record
pub struct DiagRecord {
    pub state: String, // 5-char SQLSTATE e.g. "HY000"
    pub native_error: i32,
    pub message: String,
}

/// Column descriptor
pub struct ColumnDesc {
    pub name: String,
    pub sql_type: SQLSMALLINT,
    pub size: SQLULEN,
    pub decimal_digits: SQLSMALLINT,
    pub nullable: SQLSMALLINT,
}

/// Environment handle
pub struct Environment {
    pub odbc_version: SQLINTEGER,
    pub connections: Vec<*mut Connection>,
}

/// Connection handle
pub struct Connection {
    pub env: *mut Environment,
    pub client: Option<TdsClient>,
    pub runtime: Option<tokio::runtime::Runtime>,
    pub server: String,
    pub database: String,
    pub uid: String,
    pub pwd: String,
    pub diagnostics: Vec<DiagRecord>,
    pub statements: Vec<*mut Statement>,
    pub connected: bool,
    pub autocommit: bool,
    pub in_transaction: bool,
}

/// Statement handle
pub struct Statement {
    pub conn: *mut Connection,
    pub columns: Vec<ColumnDesc>,
    pub rows: Vec<Vec<CellValue>>,
    pub row_index: isize, // -1 = before first row
    pub diagnostics: Vec<DiagRecord>,
    pub executed: bool,
    pub prepared_sql: Option<String>,
    pub row_count: SQLLEN,
    pub bound_params: Vec<BoundParam>,
    pub read_offsets: Vec<usize>,
    pub paramset_size: usize,
    // DAE (data-at-execution) state
    pub dae_sql: Option<String>,
    pub dae_params_needed: Vec<u16>,
    pub dae_current_idx: usize,
    pub dae_collected: Vec<(u16, String)>,
    pub dae_current_buf: Vec<u8>,
    // Multiple result sets — not used in streaming mode
    pub pending_result_sets: Vec<ResultSet>,
    // Streaming state
    pub streaming: bool,
    pub current_row: Vec<CellValue>,
    pub prefetch_buffer: VecDeque<Vec<CellValue>>,
    pub prefetch_done: Option<PrefetchTerminal>,
}

/// Terminal state saved when prefetch batch hits end-of-stream
pub enum PrefetchTerminal {
    Done,
    MoreResults,
    Error(String),
}

/// A bound parameter
#[allow(dead_code)]
pub struct BoundParam {
    pub param_number: u16,
    pub value_type: SQLSMALLINT,
    pub parameter_type: SQLSMALLINT,
    pub column_size: SQLULEN,
    pub decimal_digits: SQLSMALLINT,
    pub value_ptr: SQLPOINTER,
    pub buffer_length: SQLLEN,
    pub len_ind_ptr: *mut SQLLEN,
}

/// A single result set (columns + rows)
pub struct ResultSet {
    pub columns: Vec<ColumnDesc>,
    pub rows: Vec<Vec<CellValue>>,
    pub done_rows: u64,
}

// ── Helper: convert DecimalParts to i128 ────────────────────────────

fn decimal_parts_to_i128(parts: &DecimalParts) -> i128 {
    let u128_value = parts
        .int_parts
        .iter()
        .enumerate()
        .fold(0u128, |acc, (i, &part)| {
            acc + ((part as u32 as u128) << (i * 32))
        });
    if parts.is_positive {
        u128_value as i128
    } else {
        -(u128_value as i128)
    }
}

// ── Helper: convert SqlDate (days since 0001-01-01) to epoch days ───

/// SqlDate stores days since 0001-01-01.
/// Our CellValue::Date stores days since 1970-01-01 (Unix epoch).
/// Offset: 1970-01-01 is day 719162 since 0001-01-01.
const EPOCH_OFFSET_DAYS: i32 = 719162;

fn sql_date_to_epoch_days(date: &SqlDate) -> i32 {
    date.get_days() as i32 - EPOCH_OFFSET_DAYS
}

// ── Helper: convert SqlDateTime to micros since epoch ───────────────

/// SqlDateTime: days since 1900-01-01, time in 1/300s since midnight
/// 1900-01-01 is day -25567 from Unix epoch (1970-01-01)
const DATETIME_EPOCH_OFFSET_DAYS: i64 = -25567;

fn sql_datetime_to_micros(dt: &SqlDateTime) -> i64 {
    let days = dt.days as i64 + DATETIME_EPOCH_OFFSET_DAYS;
    let time_micros = (dt.time as i64) * 1_000_000 / 300;
    days * 86_400_000_000 + time_micros
}

/// SqlSmallDateTime: days (u16) since 1900-01-01, time in minutes since midnight
fn sql_smalldatetime_to_micros(dt: &SqlSmallDateTime) -> i64 {
    let days = dt.days as i64 + DATETIME_EPOCH_OFFSET_DAYS;
    let time_micros = dt.time as i64 * 60 * 1_000_000;
    days * 86_400_000_000 + time_micros
}

/// SqlDateTime2: days since 0001-01-01, time in nanoseconds
fn sql_datetime2_to_micros(dt2: &SqlDateTime2) -> i64 {
    let days = dt2.days as i64 - EPOCH_OFFSET_DAYS as i64;
    let time_micros = (dt2.time.time_nanoseconds / 1000) as i64;
    days * 86_400_000_000 + time_micros
}

/// SqlMoney to f64
fn sql_money_to_f64(m: &SqlMoney) -> f64 {
    let lsb_in_i64 = (m.lsb_part as i64) & 0x00000000FFFFFFFF;
    let money_val = lsb_in_i64 | ((m.msb_part as i64) << 32);
    money_val as f64 / 10000.0
}

// ── Map ColumnMetadata to ODBC SQL types ────────────────────────────

pub fn sql_type_from_metadata(c: &ColumnMetadata) -> (SQLSMALLINT, SQLULEN, SQLSMALLINT, SQLSMALLINT) {
    let sql_type = match c.data_type {
        TdsDataType::Int4 => SQL_INTEGER,
        TdsDataType::Int2 => SQL_SMALLINT,
        TdsDataType::Int1 => SQL_TINYINT,
        TdsDataType::Int8 => SQL_BIGINT,
        TdsDataType::IntN => {
            // IntN: determine from length
            match c.type_info.length {
                1 => SQL_TINYINT,
                2 => SQL_SMALLINT,
                4 => SQL_INTEGER,
                8 => SQL_BIGINT,
                _ => SQL_BIGINT,
            }
        }
        TdsDataType::Flt8 => SQL_DOUBLE,
        TdsDataType::Flt4 => SQL_REAL,
        TdsDataType::FltN => {
            match c.type_info.length {
                4 => SQL_REAL,
                _ => SQL_DOUBLE,
            }
        }
        TdsDataType::Bit | TdsDataType::BitN => SQL_BIT,
        TdsDataType::BigVarChar | TdsDataType::VarChar => SQL_VARCHAR,
        TdsDataType::NVarChar => SQL_WVARCHAR,
        TdsDataType::BigChar | TdsDataType::Char => SQL_CHAR,
        TdsDataType::NChar => SQL_WCHAR,
        TdsDataType::Text => SQL_LONGVARCHAR,
        TdsDataType::NText => SQL_WLONGVARCHAR,
        TdsDataType::BigBinary | TdsDataType::Binary => SQL_BINARY,
        TdsDataType::BigVarBinary | TdsDataType::VarBinary | TdsDataType::Image => SQL_VARBINARY,
        TdsDataType::DecimalN | TdsDataType::Decimal => SQL_DECIMAL,
        TdsDataType::NumericN | TdsDataType::Numeric => SQL_NUMERIC,
        TdsDataType::Money | TdsDataType::MoneyN | TdsDataType::Money4 => SQL_DECIMAL,
        TdsDataType::DateTime | TdsDataType::DateTimeN | TdsDataType::DateTim4 | TdsDataType::DateTime2N => SQL_TYPE_TIMESTAMP,
        TdsDataType::DateN => SQL_TYPE_DATE,
        TdsDataType::TimeN => SQL_TYPE_TIME,
        TdsDataType::DateTimeOffsetN => SQL_TYPE_TIMESTAMP,
        TdsDataType::Guid => SQL_GUID,
        _ => SQL_VARCHAR,
    };

    let nullable = if c.is_nullable() {
        SQL_NULLABLE
    } else {
        SQL_NO_NULLS
    };

    use mssql_tds::datatypes::sqldatatypes::TypeInfoVariant;

    let mut decimal_digits: SQLSMALLINT = 0;
    let size: SQLULEN = match sql_type {
        SQL_INTEGER => 10,
        SQL_SMALLINT => 5,
        SQL_TINYINT => 3,
        SQL_BIGINT => 19,
        SQL_DOUBLE => 53,
        SQL_REAL => 24,
        SQL_BIT => 1,
        SQL_TYPE_TIMESTAMP => 23,
        SQL_TYPE_DATE => 10,
        SQL_TYPE_TIME => 16,
        SQL_GUID => 36,
        SQL_DECIMAL | SQL_NUMERIC => {
            if let TypeInfoVariant::VarLenPrecisionScale(_, _, precision, scale) = &c.type_info.type_info_variant {
                decimal_digits = *scale as SQLSMALLINT;
                *precision as SQLULEN
            } else {
                38
            }
        }
        SQL_WVARCHAR | SQL_WCHAR | SQL_WLONGVARCHAR => {
            match &c.type_info.type_info_variant {
                TypeInfoVariant::VarLenString(_, len, _) => {
                    if *len >= 0xfffffffe { 0 } else { *len / 2 }
                }
                TypeInfoVariant::PartialLen(_, len, _, _, _) => {
                    len.map(|l| if l >= 0xfffffffe { 0 } else { l / 2 }).unwrap_or(256)
                }
                _ => 256,
            }
        }
        SQL_VARCHAR | SQL_CHAR | SQL_LONGVARCHAR => {
            match &c.type_info.type_info_variant {
                TypeInfoVariant::VarLenString(_, len, _) => {
                    if *len >= 0xfffffffe { 0 } else { *len }
                }
                TypeInfoVariant::PartialLen(_, len, _, _, _) => {
                    len.map(|l| if l >= 0xfffffffe { 0 } else { l }).unwrap_or(256)
                }
                _ => 256,
            }
        }
        SQL_BINARY | SQL_VARBINARY | SQL_LONGVARBINARY => {
            match &c.type_info.type_info_variant {
                TypeInfoVariant::VarLen(_, len) => {
                    if *len >= 0xfffffffe { 0 } else { *len }
                }
                TypeInfoVariant::PartialLen(_, len, _, _, _) => {
                    len.map(|l| if l >= 0xfffffffe { 0 } else { l }).unwrap_or(256)
                }
                _ => c.type_info.length,
            }
        }
        _ => 256,
    };

    (sql_type, size, decimal_digits, nullable)
}

// ── StringRowWriter: collects full result sets ──────────────────────

pub struct StringRowWriter {
    pub result_sets: Vec<ResultSet>,
    current_columns: Vec<ColumnDesc>,
    current_rows: Vec<Vec<CellValue>>,
    current_row: Vec<CellValue>,
    got_metadata: bool,
    pub done_rows: u64,
    pub info_messages: Vec<(u32, String)>,
}

impl StringRowWriter {
    pub fn new() -> Self {
        Self {
            result_sets: Vec::new(),
            current_columns: Vec::new(),
            current_rows: Vec::new(),
            current_row: Vec::new(),
            got_metadata: false,
            done_rows: 0,
            info_messages: Vec::new(),
        }
    }

    /// Set metadata from ColumnMetadata slice
    pub fn set_metadata(&mut self, columns: &[ColumnMetadata]) {
        if self.got_metadata {
            self.result_sets.push(ResultSet {
                columns: std::mem::take(&mut self.current_columns),
                rows: std::mem::take(&mut self.current_rows),
                done_rows: self.done_rows,
            });
            self.done_rows = 0;
        }
        self.got_metadata = true;
        self.current_columns = columns
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
    }

    /// Finalize: flush any pending result set
    #[allow(dead_code)]
    pub fn finalize(&mut self) {
        if self.got_metadata {
            self.result_sets.push(ResultSet {
                columns: std::mem::take(&mut self.current_columns),
                rows: std::mem::take(&mut self.current_rows),
                done_rows: self.done_rows,
            });
            self.got_metadata = false;
            self.done_rows = 0;
        }
    }

    /// Take the current columns and rows (for exec_direct use)
    pub fn take_current(&mut self) -> (Vec<ColumnDesc>, Vec<Vec<CellValue>>) {
        (
            std::mem::take(&mut self.current_columns),
            std::mem::take(&mut self.current_rows),
        )
    }
}

impl RowWriter for StringRowWriter {
    fn write_null(&mut self, _col: usize) {
        self.current_row.push(CellValue::Null);
    }
    fn write_bool(&mut self, _col: usize, val: bool) {
        self.current_row.push(CellValue::Bool(val));
    }
    fn write_u8(&mut self, _col: usize, val: u8) {
        self.current_row.push(CellValue::U8(val));
    }
    fn write_i16(&mut self, _col: usize, val: i16) {
        self.current_row.push(CellValue::I16(val));
    }
    fn write_i32(&mut self, _col: usize, val: i32) {
        self.current_row.push(CellValue::I32(val));
    }
    fn write_i64(&mut self, _col: usize, val: i64) {
        self.current_row.push(CellValue::I64(val));
    }
    fn write_f32(&mut self, _col: usize, val: f32) {
        self.current_row.push(CellValue::F32(val));
    }
    fn write_f64(&mut self, _col: usize, val: f64) {
        self.current_row.push(CellValue::F64(val));
    }
    fn write_string(&mut self, _col: usize, val: SqlString) {
        self.current_row
            .push(CellValue::String(val.to_utf8_string()));
    }
    fn write_bytes(&mut self, _col: usize, val: Vec<u8>) {
        self.current_row.push(CellValue::Bytes(val));
    }
    fn write_decimal(&mut self, _col: usize, val: DecimalParts) {
        self.current_row.push(CellValue::Decimal {
            value: decimal_parts_to_i128(&val),
            precision: val.precision,
            scale: val.scale,
        });
    }
    fn write_numeric(&mut self, _col: usize, val: DecimalParts) {
        self.current_row.push(CellValue::Decimal {
            value: decimal_parts_to_i128(&val),
            precision: val.precision,
            scale: val.scale,
        });
    }
    fn write_date(&mut self, _col: usize, val: SqlDate) {
        self.current_row.push(CellValue::Date {
            days: sql_date_to_epoch_days(&val),
        });
    }
    fn write_time(&mut self, _col: usize, val: SqlTime) {
        self.current_row.push(CellValue::Time {
            nanos: val.time_nanoseconds as i64,
        });
    }
    fn write_datetime(&mut self, _col: usize, val: SqlDateTime) {
        self.current_row.push(CellValue::DateTime {
            micros: sql_datetime_to_micros(&val),
        });
    }
    fn write_smalldatetime(&mut self, _col: usize, val: SqlSmallDateTime) {
        self.current_row.push(CellValue::DateTime {
            micros: sql_smalldatetime_to_micros(&val),
        });
    }
    fn write_datetime2(&mut self, _col: usize, val: SqlDateTime2) {
        self.current_row.push(CellValue::DateTime {
            micros: sql_datetime2_to_micros(&val),
        });
    }
    fn write_datetimeoffset(&mut self, _col: usize, val: SqlDateTimeOffset) {
        self.current_row.push(CellValue::DateTimeOffset {
            micros: sql_datetime2_to_micros(&val.datetime2),
            offset_min: val.offset,
        });
    }
    fn write_money(&mut self, _col: usize, val: SqlMoney) {
        // Money is stored as f64 * 10000 fixed point
        let f = sql_money_to_f64(&val);
        self.current_row.push(CellValue::F64(f));
    }
    fn write_smallmoney(&mut self, _col: usize, val: SqlSmallMoney) {
        let f = val.int_val as f64 / 10000.0;
        self.current_row.push(CellValue::F64(f));
    }
    fn write_uuid(&mut self, _col: usize, val: Uuid) {
        self.current_row.push(CellValue::Guid(*val.as_bytes()));
    }
    fn write_xml(&mut self, _col: usize, val: SqlXml) {
        self.current_row
            .push(CellValue::String(val.as_string()));
    }
    fn write_json(&mut self, _col: usize, val: SqlJson) {
        self.current_row
            .push(CellValue::String(val.as_string()));
    }
    fn write_vector(&mut self, _col: usize, _val: SqlVector) {
        // Vectors: store as string representation
        self.current_row
            .push(CellValue::String("[vector]".to_string()));
    }
    fn end_row(&mut self) {
        if self.got_metadata {
            let row = std::mem::replace(
                &mut self.current_row,
                Vec::with_capacity(self.current_columns.len()),
            );
            self.current_rows.push(row);
        }
    }
}

// ── SingleRowWriter: for streaming fetch ────────────────────────────

pub struct SingleRowWriter<'a> {
    pub row: &'a mut Vec<CellValue>,
}

impl<'a> RowWriter for SingleRowWriter<'a> {
    fn write_null(&mut self, _col: usize) {
        self.row.push(CellValue::Null);
    }
    fn write_bool(&mut self, _col: usize, val: bool) {
        self.row.push(CellValue::Bool(val));
    }
    fn write_u8(&mut self, _col: usize, val: u8) {
        self.row.push(CellValue::U8(val));
    }
    fn write_i16(&mut self, _col: usize, val: i16) {
        self.row.push(CellValue::I16(val));
    }
    fn write_i32(&mut self, _col: usize, val: i32) {
        self.row.push(CellValue::I32(val));
    }
    fn write_i64(&mut self, _col: usize, val: i64) {
        self.row.push(CellValue::I64(val));
    }
    fn write_f32(&mut self, _col: usize, val: f32) {
        self.row.push(CellValue::F32(val));
    }
    fn write_f64(&mut self, _col: usize, val: f64) {
        self.row.push(CellValue::F64(val));
    }
    fn write_string(&mut self, _col: usize, val: SqlString) {
        self.row
            .push(CellValue::String(val.to_utf8_string()));
    }
    fn write_bytes(&mut self, _col: usize, val: Vec<u8>) {
        self.row.push(CellValue::Bytes(val));
    }
    fn write_decimal(&mut self, _col: usize, val: DecimalParts) {
        self.row.push(CellValue::Decimal {
            value: decimal_parts_to_i128(&val),
            precision: val.precision,
            scale: val.scale,
        });
    }
    fn write_numeric(&mut self, _col: usize, val: DecimalParts) {
        self.row.push(CellValue::Decimal {
            value: decimal_parts_to_i128(&val),
            precision: val.precision,
            scale: val.scale,
        });
    }
    fn write_date(&mut self, _col: usize, val: SqlDate) {
        self.row.push(CellValue::Date {
            days: sql_date_to_epoch_days(&val),
        });
    }
    fn write_time(&mut self, _col: usize, val: SqlTime) {
        self.row.push(CellValue::Time {
            nanos: val.time_nanoseconds as i64,
        });
    }
    fn write_datetime(&mut self, _col: usize, val: SqlDateTime) {
        self.row.push(CellValue::DateTime {
            micros: sql_datetime_to_micros(&val),
        });
    }
    fn write_smalldatetime(&mut self, _col: usize, val: SqlSmallDateTime) {
        self.row.push(CellValue::DateTime {
            micros: sql_smalldatetime_to_micros(&val),
        });
    }
    fn write_datetime2(&mut self, _col: usize, val: SqlDateTime2) {
        self.row.push(CellValue::DateTime {
            micros: sql_datetime2_to_micros(&val),
        });
    }
    fn write_datetimeoffset(&mut self, _col: usize, val: SqlDateTimeOffset) {
        self.row.push(CellValue::DateTimeOffset {
            micros: sql_datetime2_to_micros(&val.datetime2),
            offset_min: val.offset,
        });
    }
    fn write_money(&mut self, _col: usize, val: SqlMoney) {
        let f = sql_money_to_f64(&val);
        self.row.push(CellValue::F64(f));
    }
    fn write_smallmoney(&mut self, _col: usize, val: SqlSmallMoney) {
        let f = val.int_val as f64 / 10000.0;
        self.row.push(CellValue::F64(f));
    }
    fn write_uuid(&mut self, _col: usize, val: Uuid) {
        self.row.push(CellValue::Guid(*val.as_bytes()));
    }
    fn write_xml(&mut self, _col: usize, val: SqlXml) {
        self.row
            .push(CellValue::String(val.as_string()));
    }
    fn write_json(&mut self, _col: usize, val: SqlJson) {
        self.row
            .push(CellValue::String(val.as_string()));
    }
    fn write_vector(&mut self, _col: usize, _val: SqlVector) {
        self.row
            .push(CellValue::String("[vector]".to_string()));
    }
    fn end_row(&mut self) {
        // no-op: row is taken by caller
    }
}

// hex encode helper
pub(crate) mod hex {
    pub fn encode(data: &[u8]) -> String {
        data.iter().map(|b| format!("{:02x}", b)).collect()
    }
}
