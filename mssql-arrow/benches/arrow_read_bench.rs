// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Criterion microbenchmark: Arrow read deserialization paths.
//!
//! Measures the cost of pushing decoded TDS values into Arrow builders vs
//! going through the ColumnValues enum intermediate step.
//!
//! No SQL Server required — simulates the RowWriter call pattern.
//!
//! Run with:
//!   cargo bench -p mssql-arrow -- arrow_read

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use arrow_array::builder::{
    Decimal128Builder, Float64Builder, Int32Builder, Int64Builder, StringBuilder,
};
use arrow_array::RecordBatch;
use arrow_schema::{DataType, Field, Schema};
use mssql_tds::datatypes::column_values::ColumnValues;
use mssql_tds::datatypes::decoder::DecimalParts;
use mssql_tds::datatypes::row_writer::{write_column_value, DefaultRowWriter, RowWriter};
use mssql_tds::datatypes::sql_string::{EncodingType, SqlString};
use std::sync::Arc;

// ── Simulated "direct" Arrow reader (mirrors ArrowQueryReader logic) ─────

struct DirectArrowReader {
    int_builder: Int32Builder,
    bigint_builder: Int64Builder,
    float_builder: Float64Builder,
    string_builder: StringBuilder,
    decimal_builder: Decimal128Builder,
    row_count: usize,
}

impl DirectArrowReader {
    fn new(capacity: usize) -> Self {
        Self {
            int_builder: Int32Builder::with_capacity(capacity),
            bigint_builder: Int64Builder::with_capacity(capacity),
            float_builder: Float64Builder::with_capacity(capacity),
            string_builder: StringBuilder::with_capacity(capacity, capacity * 20),
            decimal_builder: Decimal128Builder::with_capacity(capacity)
                .with_precision_and_scale(18, 2)
                .unwrap(),
            row_count: 0,
        }
    }

    fn finish(mut self) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int32, false),
            Field::new("amount", DataType::Int64, false),
            Field::new("price", DataType::Float64, false),
            Field::new("name", DataType::Utf8, false),
            Field::new("total", DataType::Decimal128(18, 2), false),
        ]));
        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(self.int_builder.finish()),
                Arc::new(self.bigint_builder.finish()),
                Arc::new(self.float_builder.finish()),
                Arc::new(self.string_builder.finish()),
                Arc::new(self.decimal_builder.finish()),
            ],
        )
        .unwrap()
    }
}

impl RowWriter for DirectArrowReader {
    fn write_null(&mut self, col: usize) {
        match col {
            0 => self.int_builder.append_null(),
            1 => self.bigint_builder.append_null(),
            2 => self.float_builder.append_null(),
            3 => self.string_builder.append_null(),
            4 => self.decimal_builder.append_null(),
            _ => {}
        }
    }
    fn write_bool(&mut self, _col: usize, _val: bool) {}
    fn write_u8(&mut self, _col: usize, _val: u8) {}
    fn write_i16(&mut self, _col: usize, _val: i16) {}
    fn write_i32(&mut self, col: usize, val: i32) {
        if col == 0 {
            self.int_builder.append_value(val);
        }
    }
    fn write_i64(&mut self, col: usize, val: i64) {
        if col == 1 {
            self.bigint_builder.append_value(val);
        }
    }
    fn write_f32(&mut self, _col: usize, _val: f32) {}
    fn write_f64(&mut self, col: usize, val: f64) {
        if col == 2 {
            self.float_builder.append_value(val);
        }
    }
    fn write_string(&mut self, col: usize, val: SqlString) {
        if col == 3 {
            self.string_builder.append_value(val.to_string());
        }
    }
    fn write_bytes(&mut self, _col: usize, _val: Vec<u8>) {}
    fn write_decimal(&mut self, col: usize, val: DecimalParts) {
        if col == 4 {
            let mut value: i128 = 0;
            for (i, &part) in val.int_parts.iter().enumerate() {
                value |= ((part as u32) as i128) << (i * 32);
            }
            if !val.is_positive {
                value = -value;
            }
            self.decimal_builder.append_value(value);
        }
    }
    fn write_numeric(&mut self, col: usize, val: DecimalParts) {
        self.write_decimal(col, val);
    }
    fn write_date(&mut self, _col: usize, _val: mssql_tds::datatypes::column_values::SqlDate) {}
    fn write_time(&mut self, _col: usize, _val: mssql_tds::datatypes::column_values::SqlTime) {}
    fn write_datetime(
        &mut self,
        _col: usize,
        _val: mssql_tds::datatypes::column_values::SqlDateTime,
    ) {
    }
    fn write_smalldatetime(
        &mut self,
        _col: usize,
        _val: mssql_tds::datatypes::column_values::SqlSmallDateTime,
    ) {
    }
    fn write_datetime2(
        &mut self,
        _col: usize,
        _val: mssql_tds::datatypes::column_values::SqlDateTime2,
    ) {
    }
    fn write_datetimeoffset(
        &mut self,
        _col: usize,
        _val: mssql_tds::datatypes::column_values::SqlDateTimeOffset,
    ) {
    }
    fn write_money(
        &mut self,
        _col: usize,
        _val: mssql_tds::datatypes::column_values::SqlMoney,
    ) {
    }
    fn write_smallmoney(
        &mut self,
        _col: usize,
        _val: mssql_tds::datatypes::column_values::SqlSmallMoney,
    ) {
    }
    fn write_uuid(&mut self, _col: usize, _val: uuid::Uuid) {}
    fn write_xml(
        &mut self,
        _col: usize,
        _val: mssql_tds::datatypes::column_values::SqlXml,
    ) {
    }
    fn write_json(
        &mut self,
        _col: usize,
        _val: mssql_tds::datatypes::sql_json::SqlJson,
    ) {
    }
    fn write_vector(
        &mut self,
        _col: usize,
        _val: mssql_tds::datatypes::sql_vector::SqlVector,
    ) {
    }
    fn end_row(&mut self) {
        self.row_count += 1;
    }
}

// ── Generate simulated decoded values ────────────────────────────────────

fn make_string(i: usize) -> SqlString {
    SqlString::new(
        format!("product-name-{i:06}").encode_utf16().flat_map(|c| c.to_le_bytes()).collect(),
        EncodingType::Utf16,
    )
}

fn make_decimal(i: usize) -> DecimalParts {
    DecimalParts {
        is_positive: true,
        scale: 2,
        precision: 18,
        int_parts: vec![(i as i64 * 10000 + 12345) as i32],
    }
}

// ── Benchmark: Direct Arrow via RowWriter ────────────────────────────────

fn bench_direct_arrow(num_rows: usize) -> RecordBatch {
    let mut reader = DirectArrowReader::new(num_rows);

    for i in 0..num_rows {
        reader.write_i32(0, i as i32);
        reader.write_i64(1, (i as i64) * 100 + 42);
        reader.write_f64(2, 99.95 + i as f64 * 0.01);
        reader.write_string(3, make_string(i));
        reader.write_decimal(4, make_decimal(i));
        reader.end_row();
    }

    reader.finish()
}

// ── Benchmark: Materialized → Arrow ──────────────────────────────────────

fn bench_materialized_arrow(num_rows: usize) -> RecordBatch {
    // Phase 1: Simulate decoder producing ColumnValues via DefaultRowWriter
    let mut all_rows: Vec<Vec<ColumnValues>> = Vec::with_capacity(num_rows);

    for i in 0..num_rows {
        let mut writer = DefaultRowWriter::new(5);
        writer.write_i32(0, i as i32);
        writer.write_i64(1, (i as i64) * 100 + 42);
        writer.write_f64(2, 99.95 + i as f64 * 0.01);
        writer.write_string(3, make_string(i));
        writer.write_decimal(4, make_decimal(i));
        writer.end_row();
        all_rows.push(writer.take_row());
    }

    // Phase 2: Convert to Arrow
    let mut id_builder = Int32Builder::with_capacity(num_rows);
    let mut amount_builder = Int64Builder::with_capacity(num_rows);
    let mut price_builder = Float64Builder::with_capacity(num_rows);
    let mut name_builder = StringBuilder::with_capacity(num_rows, num_rows * 20);
    let mut total_builder = Decimal128Builder::with_capacity(num_rows)
        .with_precision_and_scale(18, 2)
        .unwrap();

    for row in &all_rows {
        if let ColumnValues::Int(v) = &row[0] {
            id_builder.append_value(*v);
        }
        if let ColumnValues::BigInt(v) = &row[1] {
            amount_builder.append_value(*v);
        }
        if let ColumnValues::Float(v) = &row[2] {
            price_builder.append_value(*v);
        }
        if let ColumnValues::String(v) = &row[3] {
            name_builder.append_value(v.to_string());
        }
        if let ColumnValues::Decimal(parts) = &row[4] {
            let mut value: i128 = 0;
            for (i, &part) in parts.int_parts.iter().enumerate() {
                value |= ((part as u32) as i128) << (i * 32);
            }
            if !parts.is_positive {
                value = -value;
            }
            total_builder.append_value(value);
        }
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new("amount", DataType::Int64, false),
        Field::new("price", DataType::Float64, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("total", DataType::Decimal128(18, 2), false),
    ]));

    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(id_builder.finish()),
            Arc::new(amount_builder.finish()),
            Arc::new(price_builder.finish()),
            Arc::new(name_builder.finish()),
            Arc::new(total_builder.finish()),
        ],
    )
    .unwrap()
}

// ── Criterion harness ────────────────────────────────────────────────────

fn arrow_read_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("arrow_read");

    for num_rows in [10_000, 100_000] {
        group.bench_with_input(
            BenchmarkId::new("direct", num_rows),
            &num_rows,
            |b, &n| {
                b.iter(|| bench_direct_arrow(n));
            },
        );

        group.bench_with_input(
            BenchmarkId::new("materialized", num_rows),
            &num_rows,
            |b, &n| {
                b.iter(|| bench_materialized_arrow(n));
            },
        );
    }

    group.finish();
}

criterion_group!(benches, arrow_read_bench);
criterion_main!(benches);
