// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synchronous row decoder that reads directly from a byte slice.
//! Bypasses `#[async_trait]` boxing overhead by using concrete types
//! with inline methods instead of trait dispatch.

use crate::core::TdsResult;
use crate::datatypes::column_values::{
    SqlDate, SqlDateTime, SqlDateTime2, SqlDateTimeOffset, SqlMoney, SqlSmallDateTime,
    SqlSmallMoney, SqlTime,
};
use crate::datatypes::decoder::DecimalParts;
use crate::datatypes::row_writer::RowWriter;
use crate::datatypes::sql_string::{SqlString, get_encoding_type};
use crate::datatypes::sqldatatypes::{TdsDataType, TypeInfoVariant};
use crate::query::metadata::ColumnMetadata;
use byteorder::{ByteOrder, LittleEndian};

/// A zero-cost cursor over a byte slice for synchronous TDS decoding.
pub(crate) struct SyncReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

/// Error returned when the sync reader runs out of buffered data.
/// Caller should fall back to the async decode path.
#[derive(Debug)]
pub(crate) struct BufferExhausted;

impl<'a> SyncReader<'a> {
    #[inline(always)]
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Returns the number of bytes consumed so far.
    #[inline(always)]
    pub fn consumed(&self) -> usize {
        self.pos
    }

    #[inline(always)]
    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    #[inline(always)]
    fn ensure(&self, n: usize) -> Result<(), BufferExhausted> {
        if self.remaining() >= n {
            Ok(())
        } else {
            Err(BufferExhausted)
        }
    }

    #[inline(always)]
    pub fn read_byte(&mut self) -> Result<u8, BufferExhausted> {
        self.ensure(1)?;
        let v = self.buf[self.pos];
        self.pos += 1;
        Ok(v)
    }

    #[inline(always)]
    pub fn read_i16(&mut self) -> Result<i16, BufferExhausted> {
        self.ensure(2)?;
        let v = LittleEndian::read_i16(&self.buf[self.pos..]);
        self.pos += 2;
        Ok(v)
    }

    #[inline(always)]
    pub fn read_u16(&mut self) -> Result<u16, BufferExhausted> {
        self.ensure(2)?;
        let v = LittleEndian::read_u16(&self.buf[self.pos..]);
        self.pos += 2;
        Ok(v)
    }

    #[inline(always)]
    pub fn read_i32(&mut self) -> Result<i32, BufferExhausted> {
        self.ensure(4)?;
        let v = LittleEndian::read_i32(&self.buf[self.pos..]);
        self.pos += 4;
        Ok(v)
    }

    #[inline(always)]
    pub fn read_u32(&mut self) -> Result<u32, BufferExhausted> {
        self.ensure(4)?;
        let v = LittleEndian::read_u32(&self.buf[self.pos..]);
        self.pos += 4;
        Ok(v)
    }

    #[inline(always)]
    pub fn read_i64(&mut self) -> Result<i64, BufferExhausted> {
        self.ensure(8)?;
        let v = LittleEndian::read_i64(&self.buf[self.pos..]);
        self.pos += 8;
        Ok(v)
    }

    #[inline(always)]
    pub fn read_f32(&mut self) -> Result<f32, BufferExhausted> {
        self.ensure(4)?;
        let v = LittleEndian::read_f32(&self.buf[self.pos..]);
        self.pos += 4;
        Ok(v)
    }

    #[inline(always)]
    pub fn read_f64(&mut self) -> Result<f64, BufferExhausted> {
        self.ensure(8)?;
        let v = LittleEndian::read_f64(&self.buf[self.pos..]);
        self.pos += 8;
        Ok(v)
    }

    #[inline(always)]
    pub fn read_bytes_into(&mut self, dest: &mut [u8]) -> Result<(), BufferExhausted> {
        let n = dest.len();
        self.ensure(n)?;
        dest.copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(())
    }

    #[inline(always)]
    pub fn read_bytes_vec(&mut self, n: usize) -> Result<Vec<u8>, BufferExhausted> {
        self.ensure(n)?;
        let v = self.buf[self.pos..self.pos + n].to_vec();
        self.pos += n;
        Ok(v)
    }

    #[inline(always)]
    pub fn skip(&mut self, n: usize) -> Result<(), BufferExhausted> {
        self.ensure(n)?;
        self.pos += n;
        Ok(())
    }

    #[inline(always)]
    pub fn read_u24(&mut self) -> Result<u32, BufferExhausted> {
        self.ensure(3)?;
        let v = LittleEndian::read_uint(&self.buf[self.pos..], 3) as u32;
        self.pos += 3;
        Ok(v)
    }

    #[inline(always)]
    pub fn read_uint40(&mut self) -> Result<u64, BufferExhausted> {
        self.ensure(5)?;
        let v = LittleEndian::read_uint(&self.buf[self.pos..], 5);
        self.pos += 5;
        Ok(v)
    }
}

/// Maximum allocation size for a single value (100MB).
const MAX_ALLOC_SIZE: usize = 100 * 1024 * 1024;

/// Decode a single column value synchronously from the buffer into the RowWriter.
/// Returns `Err(BufferExhausted)` if there isn't enough data in the buffer.
#[inline]
pub(crate) fn decode_column_sync<W: RowWriter + ?Sized>(
    reader: &mut SyncReader<'_>,
    metadata: &ColumnMetadata,
    col: usize,
    writer: &mut W,
) -> Result<(), BufferExhausted> {
    match metadata.data_type {
        // === Fixed-length integer types ===
        TdsDataType::Int1 => {
            writer.write_u8(col, reader.read_byte()?);
        }
        TdsDataType::Int2 => {
            writer.write_i16(col, reader.read_i16()?);
        }
        TdsDataType::Int4 => {
            writer.write_i32(col, reader.read_i32()?);
        }
        TdsDataType::Int8 => {
            writer.write_i64(col, reader.read_i64()?);
        }
        TdsDataType::IntN => {
            let byte_len = reader.read_byte()?;
            match byte_len {
                1 => writer.write_u8(col, reader.read_byte()?),
                2 => writer.write_i16(col, reader.read_i16()?),
                4 => writer.write_i32(col, reader.read_i32()?),
                8 => writer.write_i64(col, reader.read_i64()?),
                0 => writer.write_null(col),
                _ => return Err(BufferExhausted), // fallback to async for error handling
            }
        }

        // === Fixed-length float types ===
        TdsDataType::Flt4 => {
            writer.write_f32(col, reader.read_f32()?);
        }
        TdsDataType::Flt8 => {
            writer.write_f64(col, reader.read_f64()?);
        }
        TdsDataType::FltN => {
            let length = reader.read_byte()?;
            match length {
                0 => writer.write_null(col),
                4 => writer.write_f32(col, reader.read_f32()?),
                _ => writer.write_f64(col, reader.read_f64()?),
            }
        }

        // === Bit types ===
        TdsDataType::Bit => {
            writer.write_bool(col, reader.read_byte()? == 1);
        }
        TdsDataType::BitN => {
            let byte_len = reader.read_byte()?;
            if byte_len > 0 {
                writer.write_bool(col, reader.read_byte()? == 1);
            } else {
                writer.write_null(col);
            }
        }

        // === Money types ===
        TdsDataType::Money4 => {
            let raw = reader.read_i32()?;
            writer.write_smallmoney(col, SqlSmallMoney { int_val: raw });
        }
        TdsDataType::Money => {
            let msb = reader.read_i32()?;
            let lsb = reader.read_i32()?;
            writer.write_money(col, SqlMoney { lsb_part: lsb, msb_part: msb });
        }
        TdsDataType::MoneyN => {
            let byte_len = reader.read_byte()?;
            match byte_len {
                4 => {
                    let raw = reader.read_i32()?;
                    writer.write_smallmoney(col, SqlSmallMoney { int_val: raw });
                }
                8 => {
                    let msb = reader.read_i32()?;
                    let lsb = reader.read_i32()?;
                    writer.write_money(col, SqlMoney { lsb_part: lsb, msb_part: msb });
                }
                0 => writer.write_null(col),
                _ => return Err(BufferExhausted),
            }
        }

        // === Decimal / Numeric ===
        TdsDataType::DecimalN => {
            match read_decimal_sync(reader, metadata)? {
                Some(val) => writer.write_decimal(col, val),
                None => writer.write_null(col),
            }
        }
        TdsDataType::NumericN => {
            match read_decimal_sync(reader, metadata)? {
                Some(val) => writer.write_numeric(col, val),
                None => writer.write_null(col),
            }
        }

        // === String types (non-PLP only in sync path) ===
        TdsDataType::NChar
        | TdsDataType::NVarChar
        | TdsDataType::BigChar
        | TdsDataType::BigVarChar => {
            let encoding_type = get_encoding_type(metadata);
            let is_utf16 = matches!(encoding_type, crate::datatypes::sql_string::EncodingType::Utf16);
            if metadata.is_plp() {
                // PLP strings can be very large and span packets — fall back to async
                return Err(BufferExhausted);
            }
            let length = reader.read_u16()? as usize;
            if length == 0xFFFF {
                writer.write_null(col);
            } else {
                let buffer = reader.read_bytes_vec(length)?;
                writer.write_string_raw(col, buffer, is_utf16);
            }
        }

        // Legacy string types — fall back to async
        TdsDataType::Char | TdsDataType::VarChar | TdsDataType::NText | TdsDataType::Text => {
            return Err(BufferExhausted);
        }

        // === Binary types ===
        TdsDataType::BigBinary => {
            let length = reader.read_u16()? as usize;
            if length > MAX_ALLOC_SIZE {
                return Err(BufferExhausted);
            }
            let bytes = reader.read_bytes_vec(length)?;
            writer.write_bytes(col, bytes);
        }
        TdsDataType::BigVarBinary => {
            if metadata.is_plp() {
                return Err(BufferExhausted); // PLP — fall back to async
            }
            let length = reader.read_u16()? as usize;
            if length == 0xFFFF {
                writer.write_null(col);
            } else {
                let bytes = reader.read_bytes_vec(length)?;
                writer.write_bytes(col, bytes);
            }
        }

        // === DateTime types ===
        TdsDataType::DateTime => {
            let daypart = reader.read_i32()?;
            let timepart = reader.read_u32()?;
            writer.write_datetime(col, SqlDateTime { days: daypart, time: timepart });
        }
        TdsDataType::DateTim4 => {
            let daypart = reader.read_u16()?;
            let timepart = reader.read_u16()?;
            writer.write_smalldatetime(col, SqlSmallDateTime { days: daypart, time: timepart });
        }
        TdsDataType::DateTimeN => {
            let length = reader.read_byte()?;
            match length {
                0 => writer.write_null(col),
                4 => {
                    let daypart = reader.read_u16()?;
                    let timepart = reader.read_u16()?;
                    writer.write_smalldatetime(col, SqlSmallDateTime { days: daypart, time: timepart });
                }
                _ => {
                    let daypart = reader.read_i32()?;
                    let timepart = reader.read_u32()?;
                    writer.write_datetime(col, SqlDateTime { days: daypart, time: timepart });
                }
            }
        }
        TdsDataType::DateN => {
            let length = reader.read_byte()?;
            if length == 0 {
                writer.write_null(col);
            } else {
                let days = reader.read_u24()?;
                writer.write_date(col, SqlDate::unchecked_create(days));
            }
        }
        TdsDataType::TimeN => {
            let length = reader.read_byte()?;
            if length == 0 {
                writer.write_null(col);
            } else {
                let scale = metadata.get_scale();
                let scaled_value = read_time_ticks(reader, length)?;
                let time_nanoseconds = scale_to_100ns(scaled_value, scale);
                writer.write_time(col, SqlTime { time_nanoseconds, scale });
            }
        }
        TdsDataType::DateTime2N => {
            let length = reader.read_byte()?;
            if length == 0 {
                writer.write_null(col);
            } else {
                let scale = metadata.get_scale();
                let time_len = length - 3;
                let scaled_value = read_time_ticks(reader, time_len)?;
                let time_nanoseconds = scale_to_100ns(scaled_value, scale);
                let days = reader.read_u24()?;
                writer.write_datetime2(col, SqlDateTime2 {
                    days,
                    time: SqlTime { time_nanoseconds, scale },
                });
            }
        }
        TdsDataType::DateTimeOffsetN => {
            let length = reader.read_byte()?;
            if length == 0 {
                writer.write_null(col);
            } else {
                let scale = metadata.get_scale();
                let time_len = length - 5; // 3 bytes date + 2 bytes offset
                let scaled_value = read_time_ticks(reader, time_len)?;
                let time_nanoseconds = scale_to_100ns(scaled_value, scale);
                let days = reader.read_u24()?;
                let offset = reader.read_i16()?;
                writer.write_datetimeoffset(col, SqlDateTimeOffset {
                    datetime2: SqlDateTime2 {
                        days,
                        time: SqlTime { time_nanoseconds, scale },
                    },
                    offset,
                });
            }
        }

        // === GUID ===
        TdsDataType::Guid => {
            let length = reader.read_byte()?;
            if length == 0 {
                writer.write_null(col);
            } else {
                if length != 16 {
                    return Err(BufferExhausted);
                }
                let mut bytes = [0u8; 16];
                reader.read_bytes_into(&mut bytes)?;
                match uuid::Uuid::from_slice_le(&bytes) {
                    Ok(uuid) => writer.write_uuid(col, uuid),
                    Err(_) => return Err(BufferExhausted),
                }
            }
        }

        // Anything else: fall back to async path
        _ => return Err(BufferExhausted),
    }
    Ok(())
}

/// Decode a full ROW token synchronously. Returns bytes consumed on success.
/// NOTE: Does NOT call writer.end_row() — caller is responsible.
pub(crate) fn decode_row_sync<W: RowWriter + ?Sized>(
    buf: &[u8],
    columns: &[ColumnMetadata],
    writer: &mut W,
) -> Result<usize, BufferExhausted> {
    let mut reader = SyncReader::new(buf);
    for (col, meta) in columns.iter().enumerate() {
        decode_column_sync(&mut reader, meta, col, writer)?;
    }
    Ok(reader.consumed())
}

/// Decode an NBCROW token synchronously. Returns bytes consumed on success.
/// NOTE: Does NOT call writer.end_row() — caller is responsible.
pub(crate) fn decode_nbcrow_sync<W: RowWriter + ?Sized>(
    buf: &[u8],
    columns: &[ColumnMetadata],
    writer: &mut W,
) -> Result<usize, BufferExhausted> {
    let bitmap_len = columns.len().div_ceil(8);
    let mut reader = SyncReader::new(buf);
    let bitmap = reader.read_bytes_vec(bitmap_len)?;
    for (col, meta) in columns.iter().enumerate() {
        if bitmap[col / 8] & (1 << (col % 8)) != 0 {
            writer.write_null(col);
        } else {
            decode_column_sync(&mut reader, meta, col, writer)?;
        }
    }
    Ok(reader.consumed())
}

/// Read time ticks from a variable-length time field.
#[inline]
fn read_time_ticks(reader: &mut SyncReader<'_>, byte_len: u8) -> Result<u64, BufferExhausted> {
    match byte_len {
        3 => Ok(reader.read_u24()? as u64),
        4 => Ok(reader.read_u32()? as u64),
        5 => Ok(reader.read_uint40()?),
        _ => Err(BufferExhausted),
    }
}

/// Convert a scaled time value to 100-nanosecond units.
#[inline]
fn scale_to_100ns(scaled_value: u64, scale: u8) -> u64 {
    match scale {
        0 => scaled_value * 10_000_000,
        1 => scaled_value * 1_000_000,
        2 => scaled_value * 100_000,
        3 => scaled_value * 10_000,
        4 => scaled_value * 1_000,
        5 => scaled_value * 100,
        6 => scaled_value * 10,
        7 => scaled_value,
        _ => scaled_value,
    }
}

/// Read a decimal/numeric value synchronously.
fn read_decimal_sync(
    reader: &mut SyncReader<'_>,
    metadata: &ColumnMetadata,
) -> Result<Option<DecimalParts>, BufferExhausted> {
    let byte_len = reader.read_byte()?;
    if byte_len == 0 {
        return Ok(None);
    }

    let TypeInfoVariant::VarLenPrecisionScale(_, _, precision, scale) =
        metadata.type_info.type_info_variant
    else {
        return Err(BufferExhausted); // fall back to async for error handling
    };

    let sign = reader.read_byte()?;
    let is_positive = sign == 1;
    let number_of_int_parts = ((byte_len - 1) >> 2) as usize;

    let mut int_parts = vec![0i32; number_of_int_parts];
    for i in 0..number_of_int_parts {
        int_parts[i] = reader.read_i32()?;
    }

    Ok(Some(DecimalParts {
        is_positive,
        scale,
        precision,
        int_parts,
    }))
}
