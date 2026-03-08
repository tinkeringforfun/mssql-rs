// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use arrow_array::builder::{
    BinaryBuilder, BooleanBuilder, Date32Builder, Decimal128Builder, FixedSizeBinaryBuilder,
    Float32Builder, Float64Builder, Int16Builder, Int32Builder, Int64Builder, StringBuilder,
    Time64MicrosecondBuilder, TimestampMicrosecondBuilder, UInt8Builder,
};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema, TimeUnit};

use mssql_tds::connection::tds_client::{ResultSet, TdsClient};
use mssql_tds::core::TdsResult;
use mssql_tds::datatypes::column_values::{
    SqlDate, SqlDateTime, SqlDateTime2, SqlDateTimeOffset, SqlMoney, SqlSmallDateTime,
    SqlSmallMoney, SqlTime, SqlXml,
};
use mssql_tds::datatypes::decoder::DecimalParts;
use mssql_tds::datatypes::row_writer::RowWriter;
use mssql_tds::datatypes::sql_json::SqlJson;
use mssql_tds::datatypes::sql_string::SqlString;
use mssql_tds::datatypes::sql_vector::SqlVector;
use mssql_tds::query::metadata::ColumnMetadata;
use uuid::Uuid;

use std::sync::Arc;

use crate::error::ArrowError;

// ── Constants ────────────────────────────────────────────────────────────

const DAYS_0001_TO_UNIX: i32 = 719_162;
const DAYS_1900_TO_UNIX: i32 = 25_567;
const MICROS_PER_DAY: i64 = 86_400_000_000;
const MICROS_PER_MINUTE: i64 = 60_000_000;

// ── Column type tag ──────────────────────────────────────────────────────

/// Lightweight tag identifying the Arrow builder type for a column.
/// Used for `write_null` dispatch and `finish()` — the hot-path typed
/// write methods (write_i32, write_f64, etc.) bypass this entirely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum ColType {
    Boolean,
    UInt8,
    Int16,
    Int32,
    Int64,
    Float32,
    Float64,
    Decimal128,
    Utf8,
    Binary,
    Date32,
    Time64Microsecond,
    TimestampMicrosecond,
    TimestampMicrosecondUtc,
    FixedSizeBinary16,
}

// ── Helper functions ─────────────────────────────────────────────────────

fn decimal_parts_to_i128(parts: &DecimalParts) -> i128 {
    let mut value: i128 = 0;
    for (i, &part) in parts.int_parts.iter().enumerate() {
        value |= ((part as u32) as i128) << (i * 32);
    }
    if !parts.is_positive {
        value = -value;
    }
    value
}

fn tds_date_to_arrow_date32(date: &SqlDate) -> i32 {
    date.get_days() as i32 - DAYS_0001_TO_UNIX
}

fn sql_time_to_micros(time: &SqlTime) -> i64 {
    (time.time_nanoseconds / 1_000) as i64
}

fn datetime2_to_epoch_micros(dt: &SqlDateTime2) -> i64 {
    let days_since_epoch = dt.days as i64 - DAYS_0001_TO_UNIX as i64;
    let time_micros = sql_time_to_micros(&dt.time);
    days_since_epoch * MICROS_PER_DAY + time_micros
}

fn datetime_to_epoch_micros(dt: &SqlDateTime) -> i64 {
    let days_since_epoch = dt.days as i64 - DAYS_1900_TO_UNIX as i64;
    let time_micros = (dt.time as i64 * 10_000) / 3;
    days_since_epoch * MICROS_PER_DAY + time_micros
}

fn smalldatetime_to_epoch_micros(dt: &SqlSmallDateTime) -> i64 {
    let days_since_epoch = dt.days as i64 - DAYS_1900_TO_UNIX as i64;
    let time_micros = dt.time as i64 * MICROS_PER_MINUTE;
    days_since_epoch * MICROS_PER_DAY + time_micros
}

fn sql_money_to_f64(money: &SqlMoney) -> f64 {
    let lsb_in_i64 = (money.lsb_part as i64) & 0x00000000FFFFFFFF;
    let combined = lsb_in_i64 | ((money.msb_part as i64) << 32);
    combined as f64 / 10_000.0
}

// ── ArrowQueryReader — Monomorphic Dispatch ──────────────────────────────
//
// Instead of a single `Vec<ColumnBuilder>` (a 15-variant enum matched on
// every cell write), builders are stored in per-type Vecs. A dispatch
// table maps each column index to (ColType, slot) where slot is the
// index into the type-specific Vec. The hot-path `write_*` methods index
// directly into the correct typed Vec — no enum discriminant check, no
// pattern matching, just an array index + append.

/// Per-column dispatch entry: type tag + index into the type-specific builder Vec.
#[derive(Clone, Copy)]
struct ColSlot {
    col_type: ColType,
    slot: usize,
}

/// Accumulates SQL Server query results into Arrow RecordBatch format.
///
/// Uses monomorphic dispatch: each column's builder lives in a homogeneous
/// `Vec<Builder>`, indexed by a pre-computed slot. The typed `write_*`
/// methods go straight to the right builder with zero enum matching.
pub struct ArrowQueryReader {
    // Per-column dispatch table
    dispatch: Vec<ColSlot>,
    names: Vec<String>,
    nullable: Vec<bool>,

    // Per-type builder storage (each indexed by ColSlot.slot)
    bool_builders: Vec<BooleanBuilder>,
    u8_builders: Vec<UInt8Builder>,
    i16_builders: Vec<Int16Builder>,
    i32_builders: Vec<Int32Builder>,
    i64_builders: Vec<Int64Builder>,
    f32_builders: Vec<Float32Builder>,
    f64_builders: Vec<Float64Builder>,
    dec_builders: Vec<(Decimal128Builder, u8, i8)>, // (builder, precision, scale)
    str_builders: Vec<StringBuilder>,
    bin_builders: Vec<BinaryBuilder>,
    date_builders: Vec<Date32Builder>,
    time_builders: Vec<Time64MicrosecondBuilder>,
    ts_builders: Vec<TimestampMicrosecondBuilder>,
    ts_utc_builders: Vec<TimestampMicrosecondBuilder>,
    uuid_builders: Vec<FixedSizeBinaryBuilder>,

    row_count: usize,
    batch_size: usize,
    /// Reusable buffer for UTF-16 → UTF-8 string decoding.
    string_buf: String,
}

impl ArrowQueryReader {
    /// Create a reader initialized from column metadata.
    pub fn from_metadata(metadata: &[ColumnMetadata], batch_size: usize) -> TdsResult<Self> {
        let mut reader = Self {
            dispatch: Vec::with_capacity(metadata.len()),
            names: Vec::with_capacity(metadata.len()),
            nullable: Vec::with_capacity(metadata.len()),
            bool_builders: Vec::new(),
            u8_builders: Vec::new(),
            i16_builders: Vec::new(),
            i32_builders: Vec::new(),
            i64_builders: Vec::new(),
            f32_builders: Vec::new(),
            f64_builders: Vec::new(),
            dec_builders: Vec::new(),
            str_builders: Vec::new(),
            bin_builders: Vec::new(),
            date_builders: Vec::new(),
            time_builders: Vec::new(),
            ts_builders: Vec::new(),
            ts_utc_builders: Vec::new(),
            uuid_builders: Vec::new(),
            row_count: 0,
            batch_size,
            string_buf: String::with_capacity(256),
        };

        for col in metadata {
            reader.names.push(col.column_name.clone());
            reader.nullable.push(col.is_nullable());
            let slot = reader.add_builder(col, batch_size)?;
            reader.dispatch.push(slot);
        }

        Ok(reader)
    }

    /// Allocate a typed builder for the given column metadata and return its dispatch slot.
    fn add_builder(
        &mut self,
        col: &ColumnMetadata,
        capacity: usize,
    ) -> TdsResult<ColSlot> {
        use mssql_tds::datatypes::sqldatatypes::{TdsDataType, TypeInfoVariant};

        let slot = match col.data_type {
            TdsDataType::Bit | TdsDataType::BitN => {
                let idx = self.bool_builders.len();
                self.bool_builders
                    .push(BooleanBuilder::with_capacity(capacity));
                ColSlot { col_type: ColType::Boolean, slot: idx }
            }

            TdsDataType::Int1 => {
                let idx = self.u8_builders.len();
                self.u8_builders
                    .push(UInt8Builder::with_capacity(capacity));
                ColSlot { col_type: ColType::UInt8, slot: idx }
            }

            TdsDataType::Int2 => {
                let idx = self.i16_builders.len();
                self.i16_builders
                    .push(Int16Builder::with_capacity(capacity));
                ColSlot { col_type: ColType::Int16, slot: idx }
            }

            TdsDataType::Int4 => {
                let idx = self.i32_builders.len();
                self.i32_builders
                    .push(Int32Builder::with_capacity(capacity));
                ColSlot { col_type: ColType::Int32, slot: idx }
            }

            TdsDataType::Int8 => {
                let idx = self.i64_builders.len();
                self.i64_builders
                    .push(Int64Builder::with_capacity(capacity));
                ColSlot { col_type: ColType::Int64, slot: idx }
            }

            TdsDataType::IntN => match col.type_info.length {
                1 => {
                    let idx = self.u8_builders.len();
                    self.u8_builders
                        .push(UInt8Builder::with_capacity(capacity));
                    ColSlot { col_type: ColType::UInt8, slot: idx }
                }
                2 => {
                    let idx = self.i16_builders.len();
                    self.i16_builders
                        .push(Int16Builder::with_capacity(capacity));
                    ColSlot { col_type: ColType::Int16, slot: idx }
                }
                4 => {
                    let idx = self.i32_builders.len();
                    self.i32_builders
                        .push(Int32Builder::with_capacity(capacity));
                    ColSlot { col_type: ColType::Int32, slot: idx }
                }
                _ => {
                    let idx = self.i64_builders.len();
                    self.i64_builders
                        .push(Int64Builder::with_capacity(capacity));
                    ColSlot { col_type: ColType::Int64, slot: idx }
                }
            },

            TdsDataType::Flt4 => {
                let idx = self.f32_builders.len();
                self.f32_builders
                    .push(Float32Builder::with_capacity(capacity));
                ColSlot { col_type: ColType::Float32, slot: idx }
            }

            TdsDataType::Flt8 => {
                let idx = self.f64_builders.len();
                self.f64_builders
                    .push(Float64Builder::with_capacity(capacity));
                ColSlot { col_type: ColType::Float64, slot: idx }
            }

            TdsDataType::FltN => match col.type_info.length {
                4 => {
                    let idx = self.f32_builders.len();
                    self.f32_builders
                        .push(Float32Builder::with_capacity(capacity));
                    ColSlot { col_type: ColType::Float32, slot: idx }
                }
                _ => {
                    let idx = self.f64_builders.len();
                    self.f64_builders
                        .push(Float64Builder::with_capacity(capacity));
                    ColSlot { col_type: ColType::Float64, slot: idx }
                }
            },

            TdsDataType::DecimalN
            | TdsDataType::NumericN
            | TdsDataType::Decimal
            | TdsDataType::Numeric => {
                let (precision, scale) = match col.type_info.type_info_variant {
                    TypeInfoVariant::VarLenPrecisionScale(_, _, p, s) => (p, s as i8),
                    _ => (38, 0),
                };
                let idx = self.dec_builders.len();
                self.dec_builders.push((
                    Decimal128Builder::with_capacity(capacity)
                        .with_precision_and_scale(precision, scale)
                        .map_err(ArrowError::ArrowError)?,
                    precision,
                    scale,
                ));
                ColSlot { col_type: ColType::Decimal128, slot: idx }
            }

            TdsDataType::Money | TdsDataType::Money4 | TdsDataType::MoneyN => {
                let idx = self.f64_builders.len();
                self.f64_builders
                    .push(Float64Builder::with_capacity(capacity));
                ColSlot { col_type: ColType::Float64, slot: idx }
            }

            TdsDataType::DateN => {
                let idx = self.date_builders.len();
                self.date_builders
                    .push(Date32Builder::with_capacity(capacity));
                ColSlot { col_type: ColType::Date32, slot: idx }
            }

            TdsDataType::TimeN => {
                let idx = self.time_builders.len();
                self.time_builders
                    .push(Time64MicrosecondBuilder::with_capacity(capacity));
                ColSlot { col_type: ColType::Time64Microsecond, slot: idx }
            }

            TdsDataType::DateTime2N => {
                let idx = self.ts_builders.len();
                self.ts_builders
                    .push(TimestampMicrosecondBuilder::with_capacity(capacity));
                ColSlot { col_type: ColType::TimestampMicrosecond, slot: idx }
            }

            TdsDataType::DateTimeOffsetN => {
                let idx = self.ts_utc_builders.len();
                self.ts_utc_builders
                    .push(TimestampMicrosecondBuilder::with_capacity(capacity));
                ColSlot { col_type: ColType::TimestampMicrosecondUtc, slot: idx }
            }

            TdsDataType::DateTime | TdsDataType::DateTim4 | TdsDataType::DateTimeN => {
                let idx = self.ts_builders.len();
                self.ts_builders
                    .push(TimestampMicrosecondBuilder::with_capacity(capacity));
                ColSlot { col_type: ColType::TimestampMicrosecond, slot: idx }
            }

            TdsDataType::NVarChar
            | TdsDataType::NChar
            | TdsDataType::BigVarChar
            | TdsDataType::BigChar
            | TdsDataType::Text
            | TdsDataType::NText
            | TdsDataType::VarChar
            | TdsDataType::Char
            | TdsDataType::Xml
            | TdsDataType::Json
            | TdsDataType::Vector => {
                let avg = col.type_info.length.min(200) as usize;
                let idx = self.str_builders.len();
                self.str_builders
                    .push(StringBuilder::with_capacity(capacity, capacity * avg));
                ColSlot { col_type: ColType::Utf8, slot: idx }
            }

            TdsDataType::BigVarBinary
            | TdsDataType::BigBinary
            | TdsDataType::VarBinary
            | TdsDataType::Binary
            | TdsDataType::Image => {
                let avg = col.type_info.length.min(256) as usize;
                let idx = self.bin_builders.len();
                self.bin_builders
                    .push(BinaryBuilder::with_capacity(capacity, capacity * avg));
                ColSlot { col_type: ColType::Binary, slot: idx }
            }

            TdsDataType::Guid => {
                let idx = self.uuid_builders.len();
                self.uuid_builders
                    .push(FixedSizeBinaryBuilder::new(16));
                ColSlot { col_type: ColType::FixedSizeBinary16, slot: idx }
            }

            _ => {
                return Err(mssql_tds::error::Error::TypeConversionError(format!(
                    "unsupported TDS type {:?} for Arrow conversion",
                    col.data_type
                )));
            }
        };

        Ok(slot)
    }

    pub fn row_count(&self) -> usize {
        self.row_count
    }

    pub fn is_batch_ready(&self) -> bool {
        self.row_count >= self.batch_size
    }

    /// Drain builders into a RecordBatch. Returns None if no rows accumulated.
    pub fn finish(&mut self) -> TdsResult<Option<RecordBatch>> {
        if self.row_count == 0 {
            return Ok(None);
        }

        let mut fields = Vec::with_capacity(self.dispatch.len());
        let mut arrays: Vec<ArrayRef> = Vec::with_capacity(self.dispatch.len());

        for (i, slot) in self.dispatch.iter().enumerate() {
            let name = &self.names[i];
            let nullable = self.nullable[i];
            let (field, array) = match slot.col_type {
                ColType::Boolean => {
                    let arr = self.bool_builders[slot.slot].finish();
                    (Field::new(name, DataType::Boolean, nullable), Arc::new(arr) as ArrayRef)
                }
                ColType::UInt8 => {
                    let arr = self.u8_builders[slot.slot].finish();
                    (Field::new(name, DataType::UInt8, nullable), Arc::new(arr) as ArrayRef)
                }
                ColType::Int16 => {
                    let arr = self.i16_builders[slot.slot].finish();
                    (Field::new(name, DataType::Int16, nullable), Arc::new(arr) as ArrayRef)
                }
                ColType::Int32 => {
                    let arr = self.i32_builders[slot.slot].finish();
                    (Field::new(name, DataType::Int32, nullable), Arc::new(arr) as ArrayRef)
                }
                ColType::Int64 => {
                    let arr = self.i64_builders[slot.slot].finish();
                    (Field::new(name, DataType::Int64, nullable), Arc::new(arr) as ArrayRef)
                }
                ColType::Float32 => {
                    let arr = self.f32_builders[slot.slot].finish();
                    (Field::new(name, DataType::Float32, nullable), Arc::new(arr) as ArrayRef)
                }
                ColType::Float64 => {
                    let arr = self.f64_builders[slot.slot].finish();
                    (Field::new(name, DataType::Float64, nullable), Arc::new(arr) as ArrayRef)
                }
                ColType::Decimal128 => {
                    let (ref mut builder, precision, scale) =
                        self.dec_builders[slot.slot];
                    let arr = builder.finish();
                    (
                        Field::new(name, DataType::Decimal128(precision, scale), nullable),
                        Arc::new(arr) as ArrayRef,
                    )
                }
                ColType::Utf8 => {
                    let arr = self.str_builders[slot.slot].finish();
                    (Field::new(name, DataType::Utf8, nullable), Arc::new(arr) as ArrayRef)
                }
                ColType::Binary => {
                    let arr = self.bin_builders[slot.slot].finish();
                    (Field::new(name, DataType::Binary, nullable), Arc::new(arr) as ArrayRef)
                }
                ColType::Date32 => {
                    let arr = self.date_builders[slot.slot].finish();
                    (Field::new(name, DataType::Date32, nullable), Arc::new(arr) as ArrayRef)
                }
                ColType::Time64Microsecond => {
                    let arr = self.time_builders[slot.slot].finish();
                    (
                        Field::new(name, DataType::Time64(TimeUnit::Microsecond), nullable),
                        Arc::new(arr) as ArrayRef,
                    )
                }
                ColType::TimestampMicrosecond => {
                    let arr = self.ts_builders[slot.slot].finish();
                    (
                        Field::new(
                            name,
                            DataType::Timestamp(TimeUnit::Microsecond, None),
                            nullable,
                        ),
                        Arc::new(arr) as ArrayRef,
                    )
                }
                ColType::TimestampMicrosecondUtc => {
                    let arr = self.ts_utc_builders[slot.slot].finish();
                    (
                        Field::new(
                            name,
                            DataType::Timestamp(TimeUnit::Microsecond, Some("+00:00".into())),
                            nullable,
                        ),
                        Arc::new(arr) as ArrayRef,
                    )
                }
                ColType::FixedSizeBinary16 => {
                    let arr = self.uuid_builders[slot.slot].finish();
                    (
                        Field::new(name, DataType::FixedSizeBinary(16), nullable),
                        Arc::new(arr) as ArrayRef,
                    )
                }
            };
            fields.push(field);
            arrays.push(array);
        }

        let schema = Arc::new(Schema::new(fields));
        let batch = RecordBatch::try_new(schema, arrays).map_err(ArrowError::ArrowError)?;

        self.row_count = 0;
        // Builders are consumed by finish() — caller should create a new reader for next batch.

        Ok(Some(batch))
    }

    /// Read all rows from a result set into Vec<RecordBatch>.
    pub async fn read_result_set(
        client: &mut TdsClient,
        batch_size: usize,
    ) -> TdsResult<Vec<RecordBatch>> {
        let metadata = client.get_metadata().clone();
        if metadata.is_empty() {
            return Ok(Vec::new());
        }

        let mut reader = Self::from_metadata(&metadata, batch_size)?;
        let mut batches = Vec::new();

        // Use bulk read — single metadata clone, no per-row timeout/tracing
        client.read_all_rows_into(&mut reader).await?;

        if reader.row_count() > 0
            && let Some(batch) = reader.finish()?
        {
            batches.push(batch);
        }

        Ok(batches)
    }

    /// Append null to the correct typed builder for the given column.
    #[inline]
    fn append_null_to_slot(&mut self, slot: &ColSlot) {
        match slot.col_type {
            ColType::Boolean => self.bool_builders[slot.slot].append_null(),
            ColType::UInt8 => self.u8_builders[slot.slot].append_null(),
            ColType::Int16 => self.i16_builders[slot.slot].append_null(),
            ColType::Int32 => self.i32_builders[slot.slot].append_null(),
            ColType::Int64 => self.i64_builders[slot.slot].append_null(),
            ColType::Float32 => self.f32_builders[slot.slot].append_null(),
            ColType::Float64 => self.f64_builders[slot.slot].append_null(),
            ColType::Decimal128 => self.dec_builders[slot.slot].0.append_null(),
            ColType::Utf8 => self.str_builders[slot.slot].append_null(),
            ColType::Binary => self.bin_builders[slot.slot].append_null(),
            ColType::Date32 => self.date_builders[slot.slot].append_null(),
            ColType::Time64Microsecond => self.time_builders[slot.slot].append_null(),
            ColType::TimestampMicrosecond => self.ts_builders[slot.slot].append_null(),
            ColType::TimestampMicrosecondUtc => self.ts_utc_builders[slot.slot].append_null(),
            ColType::FixedSizeBinary16 => self.uuid_builders[slot.slot].append_null(),
        }
    }
}

// ── RowWriter — Monomorphic dispatch ─────────────────────────────────────
//
// Each typed write method indexes directly into the correct per-type Vec.
// No enum discriminant check, no pattern matching — just array[slot].append.

impl RowWriter for ArrowQueryReader {
    #[inline]
    fn write_null(&mut self, col: usize) {
        let slot = self.dispatch[col];
        self.append_null_to_slot(&slot);
    }

    #[inline]
    fn write_bool(&mut self, col: usize, val: bool) {
        self.bool_builders[self.dispatch[col].slot].append_value(val);
    }

    #[inline]
    fn write_u8(&mut self, col: usize, val: u8) {
        self.u8_builders[self.dispatch[col].slot].append_value(val);
    }

    #[inline]
    fn write_i16(&mut self, col: usize, val: i16) {
        self.i16_builders[self.dispatch[col].slot].append_value(val);
    }

    #[inline]
    fn write_i32(&mut self, col: usize, val: i32) {
        self.i32_builders[self.dispatch[col].slot].append_value(val);
    }

    #[inline]
    fn write_i64(&mut self, col: usize, val: i64) {
        self.i64_builders[self.dispatch[col].slot].append_value(val);
    }

    #[inline]
    fn write_f32(&mut self, col: usize, val: f32) {
        self.f32_builders[self.dispatch[col].slot].append_value(val);
    }

    #[inline]
    fn write_f64(&mut self, col: usize, val: f64) {
        self.f64_builders[self.dispatch[col].slot].append_value(val);
    }

    #[inline]
    fn write_string(&mut self, col: usize, val: SqlString) {
        let b = &mut self.str_builders[self.dispatch[col].slot];
        if val.is_utf16() {
            self.string_buf.clear();
            let bytes = &val.bytes;
            let utf16_iter = bytes
                .chunks_exact(2)
                .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]));
            for c in char::decode_utf16(utf16_iter) {
                self.string_buf
                    .push(c.unwrap_or(char::REPLACEMENT_CHARACTER));
            }
            b.append_value(&self.string_buf);
        } else {
            b.append_value(val.to_string());
        }
    }

    #[inline]
    fn write_bytes(&mut self, col: usize, val: Vec<u8>) {
        self.bin_builders[self.dispatch[col].slot].append_value(&val);
    }

    #[inline]
    fn write_decimal(&mut self, col: usize, val: DecimalParts) {
        self.dec_builders[self.dispatch[col].slot]
            .0
            .append_value(decimal_parts_to_i128(&val));
    }

    #[inline]
    fn write_numeric(&mut self, col: usize, val: DecimalParts) {
        self.dec_builders[self.dispatch[col].slot]
            .0
            .append_value(decimal_parts_to_i128(&val));
    }

    #[inline]
    fn write_date(&mut self, col: usize, val: SqlDate) {
        self.date_builders[self.dispatch[col].slot]
            .append_value(tds_date_to_arrow_date32(&val));
    }

    #[inline]
    fn write_time(&mut self, col: usize, val: SqlTime) {
        self.time_builders[self.dispatch[col].slot]
            .append_value(sql_time_to_micros(&val));
    }

    #[inline]
    fn write_datetime(&mut self, col: usize, val: SqlDateTime) {
        self.ts_builders[self.dispatch[col].slot]
            .append_value(datetime_to_epoch_micros(&val));
    }

    #[inline]
    fn write_smalldatetime(&mut self, col: usize, val: SqlSmallDateTime) {
        self.ts_builders[self.dispatch[col].slot]
            .append_value(smalldatetime_to_epoch_micros(&val));
    }

    #[inline]
    fn write_datetime2(&mut self, col: usize, val: SqlDateTime2) {
        self.ts_builders[self.dispatch[col].slot]
            .append_value(datetime2_to_epoch_micros(&val));
    }

    #[inline]
    fn write_datetimeoffset(&mut self, col: usize, val: SqlDateTimeOffset) {
        let local_micros = datetime2_to_epoch_micros(&val.datetime2);
        let offset_micros = val.offset as i64 * MICROS_PER_MINUTE;
        self.ts_utc_builders[self.dispatch[col].slot]
            .append_value(local_micros - offset_micros);
    }

    #[inline]
    fn write_money(&mut self, col: usize, val: SqlMoney) {
        self.f64_builders[self.dispatch[col].slot]
            .append_value(sql_money_to_f64(&val));
    }

    #[inline]
    fn write_smallmoney(&mut self, col: usize, val: SqlSmallMoney) {
        self.f64_builders[self.dispatch[col].slot]
            .append_value(val.int_val as f64 / 10_000.0);
    }

    #[inline]
    fn write_uuid(&mut self, col: usize, val: Uuid) {
        let _ = self.uuid_builders[self.dispatch[col].slot].append_value(val.as_bytes());
    }

    #[inline]
    fn write_xml(&mut self, col: usize, val: SqlXml) {
        self.str_builders[self.dispatch[col].slot]
            .append_value(val.as_string());
    }

    #[inline]
    fn write_json(&mut self, col: usize, val: SqlJson) {
        self.str_builders[self.dispatch[col].slot]
            .append_value(String::from_utf8_lossy(&val.bytes));
    }

    #[inline]
    fn write_vector(&mut self, col: usize, val: SqlVector) {
        self.str_builders[self.dispatch[col].slot]
            .append_value(format!("{val:?}"));
    }

    #[inline]
    fn end_row(&mut self) {
        self.row_count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use mssql_tds::datatypes::sqldatatypes::{
        FixedLengthTypes, TdsDataType, TypeInfo, TypeInfoVariant, VariableLengthTypes,
    };
    use mssql_tds::query::metadata::ColumnMetadata;

    fn make_col(
        name: &str,
        data_type: TdsDataType,
        type_info: TypeInfo,
        nullable: bool,
    ) -> ColumnMetadata {
        ColumnMetadata {
            user_type: 0,
            flags: if nullable { 0x01 } else { 0x00 },
            type_info,
            data_type,
            column_name: name.to_string(),
            multi_part_name: None,
        }
    }

    fn fixed_type_info(dt: TdsDataType, len: usize, ft: FixedLengthTypes) -> TypeInfo {
        TypeInfo {
            tds_type: dt,
            length: len,
            type_info_variant: TypeInfoVariant::FixedLen(ft),
        }
    }

    fn varlen_type_info(dt: TdsDataType, len: usize, vt: VariableLengthTypes) -> TypeInfo {
        TypeInfo {
            tds_type: dt,
            length: len,
            type_info_variant: TypeInfoVariant::VarLen(vt, len),
        }
    }

    // ── Schema inference tests ───────────────────────────────────────────

    #[test]
    fn from_metadata_int_columns() {
        let metadata = vec![
            make_col(
                "tiny",
                TdsDataType::Int1,
                fixed_type_info(TdsDataType::Int1, 1, FixedLengthTypes::Int1),
                false,
            ),
            make_col(
                "small",
                TdsDataType::Int2,
                fixed_type_info(TdsDataType::Int2, 2, FixedLengthTypes::Int2),
                false,
            ),
            make_col(
                "regular",
                TdsDataType::Int4,
                fixed_type_info(TdsDataType::Int4, 4, FixedLengthTypes::Int4),
                true,
            ),
            make_col(
                "big",
                TdsDataType::Int8,
                fixed_type_info(TdsDataType::Int8, 8, FixedLengthTypes::Int8),
                true,
            ),
        ];

        let reader = ArrowQueryReader::from_metadata(&metadata, 100).unwrap();
        assert_eq!(reader.names, vec!["tiny", "small", "regular", "big"]);
        assert_eq!(reader.nullable, vec![false, false, true, true]);
        assert_eq!(reader.dispatch.len(), 4);
    }

    #[test]
    fn from_metadata_intn_disambiguation() {
        let metadata = vec![
            make_col(
                "u8_col",
                TdsDataType::IntN,
                varlen_type_info(TdsDataType::IntN, 1, VariableLengthTypes::IntN),
                true,
            ),
            make_col(
                "i16_col",
                TdsDataType::IntN,
                varlen_type_info(TdsDataType::IntN, 2, VariableLengthTypes::IntN),
                true,
            ),
            make_col(
                "i32_col",
                TdsDataType::IntN,
                varlen_type_info(TdsDataType::IntN, 4, VariableLengthTypes::IntN),
                true,
            ),
            make_col(
                "i64_col",
                TdsDataType::IntN,
                varlen_type_info(TdsDataType::IntN, 8, VariableLengthTypes::IntN),
                true,
            ),
        ];

        let mut reader = ArrowQueryReader::from_metadata(&metadata, 100).unwrap();

        reader.write_u8(0, 42);
        reader.write_i16(1, -100);
        reader.write_i32(2, 999);
        reader.write_i64(3, i64::MAX);
        reader.end_row();

        let batch = reader.finish().unwrap().unwrap();
        assert_eq!(batch.num_rows(), 1);
        assert_eq!(batch.num_columns(), 4);
        assert_eq!(*batch.schema().field(0).data_type(), DataType::UInt8);
        assert_eq!(*batch.schema().field(1).data_type(), DataType::Int16);
        assert_eq!(*batch.schema().field(2).data_type(), DataType::Int32);
        assert_eq!(*batch.schema().field(3).data_type(), DataType::Int64);
    }

    #[test]
    fn from_metadata_fltn_disambiguation() {
        let metadata = vec![
            make_col(
                "f32_col",
                TdsDataType::FltN,
                varlen_type_info(TdsDataType::FltN, 4, VariableLengthTypes::FltN),
                true,
            ),
            make_col(
                "f64_col",
                TdsDataType::FltN,
                varlen_type_info(TdsDataType::FltN, 8, VariableLengthTypes::FltN),
                true,
            ),
        ];

        let mut reader = ArrowQueryReader::from_metadata(&metadata, 100).unwrap();
        reader.write_f32(0, 1.5);
        reader.write_f64(1, 2.5);
        reader.end_row();

        let batch = reader.finish().unwrap().unwrap();
        assert_eq!(*batch.schema().field(0).data_type(), DataType::Float32);
        assert_eq!(*batch.schema().field(1).data_type(), DataType::Float64);
    }

    #[test]
    fn from_metadata_decimal() {
        let metadata = vec![make_col(
            "dec",
            TdsDataType::DecimalN,
            TypeInfo {
                tds_type: TdsDataType::DecimalN,
                length: 9,
                type_info_variant: TypeInfoVariant::VarLenPrecisionScale(
                    VariableLengthTypes::DecimalN,
                    9,
                    18,
                    4,
                ),
            },
            true,
        )];

        let mut reader = ArrowQueryReader::from_metadata(&metadata, 100).unwrap();
        reader.write_decimal(
            0,
            DecimalParts {
                is_positive: true,
                scale: 4,
                precision: 18,
                int_parts: vec![123456],
            },
        );
        reader.end_row();

        let batch = reader.finish().unwrap().unwrap();
        assert_eq!(
            *batch.schema().field(0).data_type(),
            DataType::Decimal128(18, 4)
        );
    }

    // ── RowWriter + finish tests ─────────────────────────────────────────

    #[test]
    fn write_nulls_across_types() {
        let metadata = vec![
            make_col(
                "int_col",
                TdsDataType::Int4,
                fixed_type_info(TdsDataType::Int4, 4, FixedLengthTypes::Int4),
                true,
            ),
            make_col(
                "str_col",
                TdsDataType::NVarChar,
                varlen_type_info(TdsDataType::NVarChar, 200, VariableLengthTypes::NVarChar),
                true,
            ),
            make_col(
                "flt_col",
                TdsDataType::Flt8,
                fixed_type_info(TdsDataType::Flt8, 8, FixedLengthTypes::Flt8),
                true,
            ),
        ];

        let mut reader = ArrowQueryReader::from_metadata(&metadata, 100).unwrap();
        reader.write_null(0);
        reader.write_null(1);
        reader.write_null(2);
        reader.end_row();

        let batch = reader.finish().unwrap().unwrap();
        assert_eq!(batch.num_rows(), 1);
        assert!(batch.column(0).is_null(0));
        assert!(batch.column(1).is_null(0));
        assert!(batch.column(2).is_null(0));
    }

    #[test]
    fn finish_empty_returns_none() {
        let metadata = vec![make_col(
            "col",
            TdsDataType::Int4,
            fixed_type_info(TdsDataType::Int4, 4, FixedLengthTypes::Int4),
            false,
        )];

        let mut reader = ArrowQueryReader::from_metadata(&metadata, 100).unwrap();
        assert!(reader.finish().unwrap().is_none());
    }

    #[test]
    fn batch_ready_at_threshold() {
        let metadata = vec![make_col(
            "id",
            TdsDataType::Int4,
            fixed_type_info(TdsDataType::Int4, 4, FixedLengthTypes::Int4),
            false,
        )];

        let mut reader = ArrowQueryReader::from_metadata(&metadata, 3).unwrap();
        assert!(!reader.is_batch_ready());

        reader.write_i32(0, 1);
        reader.end_row();
        assert!(!reader.is_batch_ready());

        reader.write_i32(0, 2);
        reader.end_row();
        assert!(!reader.is_batch_ready());

        reader.write_i32(0, 3);
        reader.end_row();
        assert!(reader.is_batch_ready());
        assert_eq!(reader.row_count(), 3);
    }

    // ── Helper function tests ────────────────────────────────────────────

    #[test]
    fn decimal_parts_positive_and_negative() {
        let pos = DecimalParts {
            is_positive: true,
            scale: 2,
            precision: 10,
            int_parts: vec![100],
        };
        assert_eq!(decimal_parts_to_i128(&pos), 100);

        let neg = DecimalParts {
            is_positive: false,
            scale: 2,
            precision: 10,
            int_parts: vec![200],
        };
        assert_eq!(decimal_parts_to_i128(&neg), -200);
    }

    #[test]
    fn decimal_parts_multi_word() {
        let parts = DecimalParts {
            is_positive: true,
            scale: 0,
            precision: 38,
            int_parts: vec![1, 1],
        };
        assert_eq!(decimal_parts_to_i128(&parts), (1_i128 << 32) + 1);
    }

    #[test]
    fn tds_date_conversion() {
        let date = SqlDate::create(730_119).unwrap();
        assert_eq!(tds_date_to_arrow_date32(&date), 10_957);
    }

    #[test]
    fn sql_time_to_micros_conversion() {
        let time = SqlTime {
            time_nanoseconds: 1_000_000_000,
            scale: 7,
        };
        assert_eq!(sql_time_to_micros(&time), 1_000_000);
    }

    #[test]
    fn datetime_conversion() {
        let dt = SqlDateTime {
            days: DAYS_1900_TO_UNIX,
            time: 300,
        };
        let micros = datetime_to_epoch_micros(&dt);
        assert_eq!(micros, 1_000_000);
    }

    #[test]
    fn smalldatetime_conversion() {
        let dt = SqlSmallDateTime {
            days: DAYS_1900_TO_UNIX as u16,
            time: 1,
        };
        assert_eq!(smalldatetime_to_epoch_micros(&dt), MICROS_PER_MINUTE);
    }

    #[test]
    fn money_conversion() {
        let m = SqlMoney {
            lsb_part: 10_000,
            msb_part: 0,
        };
        assert!((sql_money_to_f64(&m) - 1.0).abs() < 1e-10);
    }
}
