// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! End-to-end benchmark: Arrow-direct reads via RowWriter vs materialized
//! ColumnValues → Arrow conversion for query results.
//!
//! Compares two read paths:
//!   A — ArrowQueryReader (direct RowWriter): TDS decoder writes typed values
//!       straight into Arrow column builders via the RowWriter trait.
//!   B — Materialized: TDS decoder produces Vec<ColumnValues> per row (via
//!       DefaultRowWriter), then a second pass converts each ColumnValues into
//!       Arrow builder appends.
//!
//! Run with:
//!   DB_HOST=localhost DB_USERNAME=sa SQL_PASSWORD=<pw> TRUST_SERVER_CERTIFICATE=true \
//!     cargo test -p mssql-arrow --test bench_arrow_read -- --ignored --nocapture

mod common;

use arrow_array::builder::{
    Decimal128Builder, Float64Builder, Int32Builder, Int64Builder, StringBuilder,
};
use arrow_array::RecordBatch;
use arrow_schema::{DataType, Field, Schema};
use common::{begin_connection, build_tcp_datasource};
use mssql_arrow::ArrowQueryReader;
use mssql_tds::connection::tds_client::{ResultSet, TdsClient};
use mssql_tds::core::TdsResult;
use mssql_tds::datatypes::column_values::ColumnValues;
use mssql_tds::datatypes::row_writer::DefaultRowWriter;
use std::sync::Arc;
use std::time::Instant;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

const NUM_ROWS: usize = 100_000;
const NUM_ITERATIONS: usize = 10;

// ---------------------------------------------------------------------------
// Setup: create and populate a test table
// ---------------------------------------------------------------------------

async fn setup_table(client: &mut TdsClient) -> TdsResult<()> {
    client
        .execute(
            "IF OBJECT_ID('tempdb..#arrow_read_bench') IS NOT NULL DROP TABLE #arrow_read_bench"
                .to_string(),
            None,
            None,
        )
        .await?;
    client.close_query().await?;

    client
        .execute(
            "CREATE TABLE #arrow_read_bench (
                id      INT           NOT NULL,
                amount  BIGINT        NOT NULL,
                price   FLOAT         NOT NULL,
                name    NVARCHAR(200) NOT NULL,
                total   DECIMAL(18,2) NOT NULL
            )"
            .to_string(),
            None,
            None,
        )
        .await?;
    client.close_query().await?;

    // Insert rows in batches using a numbers CTE
    let batch_size = 10_000;
    let num_batches = NUM_ROWS / batch_size;

    for batch in 0..num_batches {
        let offset = batch * batch_size;
        let sql = format!(
            "WITH nums AS (
                SELECT TOP ({batch_size}) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) - 1 + {offset} AS n
                FROM sys.all_objects a CROSS JOIN sys.all_objects b
            )
            INSERT INTO #arrow_read_bench (id, amount, price, name, total)
            SELECT
                CAST(n AS INT),
                CAST(n * 100 + 42 AS BIGINT),
                99.95 + CAST(n AS FLOAT) * 0.01,
                CONCAT('product-name-', RIGHT('000000' + CAST(n AS VARCHAR(10)), 6)),
                CAST(n * 100.00 + 123.45 AS DECIMAL(18,2))
            FROM nums"
        );
        client.execute(sql, None, None).await?;
        client.close_query().await?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Path A: ArrowQueryReader (direct RowWriter)
// ---------------------------------------------------------------------------

async fn read_direct_arrow(client: &mut TdsClient) -> TdsResult<(Vec<RecordBatch>, u64)> {
    client
        .execute(
            "SELECT id, amount, price, name, total FROM #arrow_read_bench".to_string(),
            None,
            None,
        )
        .await?;

    let start = Instant::now();
    let batches = ArrowQueryReader::read_result_set(client, NUM_ROWS).await?;
    let elapsed = start.elapsed().as_millis() as u64;

    client.close_query().await?;
    Ok((batches, elapsed))
}

// ---------------------------------------------------------------------------
// Path B: Materialized ColumnValues → Arrow conversion
// ---------------------------------------------------------------------------

fn column_values_to_i128(cv: &ColumnValues) -> i128 {
    match cv {
        ColumnValues::Decimal(parts) | ColumnValues::Numeric(parts) => {
            let mut value: i128 = 0;
            for (i, &part) in parts.int_parts.iter().enumerate() {
                value |= ((part as u32) as i128) << (i * 32);
            }
            if !parts.is_positive {
                value = -value;
            }
            value
        }
        _ => 0,
    }
}

async fn read_materialized_arrow(client: &mut TdsClient) -> TdsResult<(Vec<RecordBatch>, u64)> {
    client
        .execute(
            "SELECT id, amount, price, name, total FROM #arrow_read_bench".to_string(),
            None,
            None,
        )
        .await?;

    let start = Instant::now();

    // Phase 1: Read all rows into Vec<Vec<ColumnValues>> via DefaultRowWriter
    let metadata = client.get_metadata().clone();
    let col_count = metadata.len();
    let mut all_rows: Vec<Vec<ColumnValues>> = Vec::with_capacity(NUM_ROWS);
    let mut row_writer = DefaultRowWriter::new(col_count);

    while client.next_row_into(&mut row_writer).await? {
        all_rows.push(row_writer.take_row());
    }

    // Phase 2: Convert materialized rows into Arrow RecordBatch
    let num_rows = all_rows.len();
    let mut id_builder = Int32Builder::with_capacity(num_rows);
    let mut amount_builder = Int64Builder::with_capacity(num_rows);
    let mut price_builder = Float64Builder::with_capacity(num_rows);
    let mut name_builder = StringBuilder::with_capacity(num_rows, num_rows * 20);
    let mut total_builder = Decimal128Builder::with_capacity(num_rows)
        .with_precision_and_scale(18, 2)
        .unwrap();

    for row in &all_rows {
        match &row[0] {
            ColumnValues::Int(v) => id_builder.append_value(*v),
            _ => id_builder.append_null(),
        }
        match &row[1] {
            ColumnValues::BigInt(v) => amount_builder.append_value(*v),
            _ => amount_builder.append_null(),
        }
        match &row[2] {
            ColumnValues::Float(v) => price_builder.append_value(*v),
            _ => price_builder.append_null(),
        }
        match &row[3] {
            ColumnValues::String(v) => name_builder.append_value(v.to_string()),
            _ => name_builder.append_null(),
        }
        match &row[4] {
            ColumnValues::Decimal(_) | ColumnValues::Numeric(_) => {
                total_builder.append_value(column_values_to_i128(&row[4]));
            }
            _ => total_builder.append_null(),
        }
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new("amount", DataType::Int64, false),
        Field::new("price", DataType::Float64, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("total", DataType::Decimal128(18, 2), false),
    ]));

    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(id_builder.finish()),
            Arc::new(amount_builder.finish()),
            Arc::new(price_builder.finish()),
            Arc::new(name_builder.finish()),
            Arc::new(total_builder.finish()),
        ],
    )
    .unwrap();

    let elapsed = start.elapsed().as_millis() as u64;
    client.close_query().await?;
    Ok((vec![batch], elapsed))
}

// ---------------------------------------------------------------------------
// Path C: next_row_into() → Vec<ColumnValues> (no Arrow, baseline)
// ---------------------------------------------------------------------------

async fn read_materialized_only(client: &mut TdsClient) -> TdsResult<(usize, u64)> {
    client
        .execute(
            "SELECT id, amount, price, name, total FROM #arrow_read_bench".to_string(),
            None,
            None,
        )
        .await?;

    let start = Instant::now();

    let metadata = client.get_metadata().clone();
    let col_count = metadata.len();
    let mut row_writer = DefaultRowWriter::new(col_count);
    let mut row_count = 0usize;

    while client.next_row_into(&mut row_writer).await? {
        let _row = row_writer.take_row();
        row_count += 1;
    }

    let elapsed = start.elapsed().as_millis() as u64;
    client.close_query().await?;
    Ok((row_count, elapsed))
}

// ---------------------------------------------------------------------------
// Main benchmark
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn bench_arrow_read() -> TdsResult<()> {
    let datasource = build_tcp_datasource();
    let mut client = begin_connection(&datasource).await;

    println!("\n=== Arrow Read Benchmark ===");
    println!("Rows: {NUM_ROWS}, Iterations: {NUM_ITERATIONS}");
    println!("Schema: INT, BIGINT, FLOAT, NVARCHAR(200), DECIMAL(18,2)\n");

    // Setup
    println!("Setting up test table...");
    setup_table(&mut client).await?;
    println!("Table populated with {NUM_ROWS} rows.\n");

    // --- Path A: ArrowQueryReader (direct) ---
    let mut times_direct: Vec<u64> = Vec::with_capacity(NUM_ITERATIONS);
    for i in 1..=NUM_ITERATIONS {
        let (batches, elapsed) = read_direct_arrow(&mut client).await?;
        let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
        println!(
            "  [direct]       iter {:2}: {:>8.2} ms  ({:.1} Krows/s)  [{} rows]",
            i,
            elapsed as f64,
            total_rows as f64 / elapsed as f64,
            total_rows
        );
        times_direct.push(elapsed);
    }

    // --- Path B: Materialized → Arrow ---
    let mut times_materialized: Vec<u64> = Vec::with_capacity(NUM_ITERATIONS);
    for i in 1..=NUM_ITERATIONS {
        let (batches, elapsed) = read_materialized_arrow(&mut client).await?;
        let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
        println!(
            "  [materialized] iter {:2}: {:>8.2} ms  ({:.1} Krows/s)  [{} rows]",
            i,
            elapsed as f64,
            total_rows as f64 / elapsed as f64,
            total_rows
        );
        times_materialized.push(elapsed);
    }

    // --- Path C: Materialized only (no Arrow) ---
    let mut times_raw: Vec<u64> = Vec::with_capacity(NUM_ITERATIONS);
    for i in 1..=NUM_ITERATIONS {
        let (row_count, elapsed) = read_materialized_only(&mut client).await?;
        println!(
            "  [raw_rows]     iter {:2}: {:>8.2} ms  ({:.1} Krows/s)  [{} rows]",
            i,
            elapsed as f64,
            row_count as f64 / elapsed as f64,
            row_count
        );
        times_raw.push(elapsed);
    }

    // --- Summary ---
    println!("\n=== Summary ({NUM_ROWS} rows, {NUM_ITERATIONS} iterations) ===\n");

    let avg = |t: &[u64]| t.iter().sum::<u64>() as f64 / t.len() as f64;
    let warm_avg = |t: &[u64]| {
        let warm = &t[1..];
        warm.iter().sum::<u64>() as f64 / warm.len() as f64
    };

    let avg_direct = avg(&times_direct);
    let avg_materialized = avg(&times_materialized);
    let avg_raw = avg(&times_raw);
    let warm_direct = warm_avg(&times_direct);
    let warm_materialized = warm_avg(&times_materialized);
    let warm_raw = warm_avg(&times_raw);

    println!(
        "  {:20} {:>10} {:>12} {:>15} {:>10}",
        "Path", "Avg (ms)", "Warm Avg", "Krows/s (warm)", "Speedup"
    );
    println!(
        "  {:-<20} {:-<10} {:-<12} {:-<15} {:-<10}",
        "", "", "", "", ""
    );
    println!(
        "  {:20} {:>10.1} {:>12.1} {:>15.1} {:>10}",
        "A — Direct (Arrow)",
        avg_direct,
        warm_direct,
        NUM_ROWS as f64 / warm_direct,
        format!("{:.2}×", warm_materialized / warm_direct)
    );
    println!(
        "  {:20} {:>10.1} {:>12.1} {:>15.1} {:>10}",
        "B — Materialized",
        avg_materialized,
        warm_materialized,
        NUM_ROWS as f64 / warm_materialized,
        "1.00× (base)"
    );
    println!(
        "  {:20} {:>10.1} {:>12.1} {:>15.1} {:>10}",
        "C — Raw rows (no Arr)",
        avg_raw,
        warm_raw,
        NUM_ROWS as f64 / warm_raw,
        format!("{:.2}×", warm_materialized / warm_raw)
    );

    println!(
        "\n  Arrow conversion overhead (B warm - C warm): {:.1} ms",
        warm_materialized - warm_raw
    );
    println!(
        "  Direct overhead vs raw (A warm - C warm): {:.1} ms",
        warm_direct - warm_raw
    );

    // Cleanup
    client
        .execute(
            "DROP TABLE IF EXISTS #arrow_read_bench".to_string(),
            None,
            None,
        )
        .await?;
    client.close_query().await?;

    Ok(())
}
