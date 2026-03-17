use crate::execute;
use crate::handle::*;
use crate::types::*;

pub fn get_type_info(stmt: &mut Statement, data_type: SQLSMALLINT) -> SQLRETURN {
    let _filter = if data_type == SQL_ALL_TYPES {
        String::new()
    } else {
        format!("WHERE DATA_TYPE = {}", data_type)
    };

    // Return a standard ODBC type catalog
    let sql = format!(
        "SELECT \
         TYPE_NAME = tp.name, \
         DATA_TYPE = CASE tp.name \
           WHEN 'int' THEN 4 WHEN 'smallint' THEN 5 WHEN 'tinyint' THEN -6 \
           WHEN 'bigint' THEN -5 WHEN 'float' THEN 8 WHEN 'real' THEN 7 \
           WHEN 'bit' THEN -7 WHEN 'datetime' THEN 93 WHEN 'datetime2' THEN 93 \
           WHEN 'date' THEN 91 WHEN 'time' THEN 92 \
           WHEN 'varchar' THEN 12 WHEN 'nvarchar' THEN -9 \
           WHEN 'char' THEN 1 WHEN 'nchar' THEN -8 \
           WHEN 'text' THEN -1 WHEN 'ntext' THEN -10 \
           WHEN 'binary' THEN -2 WHEN 'varbinary' THEN -3 WHEN 'image' THEN -4 \
           WHEN 'decimal' THEN 3 WHEN 'numeric' THEN 2 \
           WHEN 'money' THEN 3 WHEN 'smallmoney' THEN 3 \
           WHEN 'uniqueidentifier' THEN -11 \
           WHEN 'xml' THEN -10 \
           ELSE 12 END, \
         COLUMN_SIZE = CASE \
           WHEN tp.name IN ('int') THEN 10 \
           WHEN tp.name IN ('smallint') THEN 5 \
           WHEN tp.name IN ('tinyint') THEN 3 \
           WHEN tp.name IN ('bigint') THEN 19 \
           WHEN tp.name IN ('float') THEN 53 \
           WHEN tp.name IN ('real') THEN 24 \
           WHEN tp.name IN ('bit') THEN 1 \
           WHEN tp.name IN ('datetime','datetime2') THEN 23 \
           WHEN tp.name IN ('date') THEN 10 \
           WHEN tp.name IN ('time') THEN 16 \
           WHEN tp.name IN ('uniqueidentifier') THEN 36 \
           ELSE tp.max_length END, \
         LITERAL_PREFIX = CASE WHEN tp.name IN ('varchar','nvarchar','char','nchar','text','ntext','datetime','datetime2','date','time','uniqueidentifier') THEN '''' WHEN tp.name IN ('binary','varbinary','image') THEN '0x' ELSE NULL END, \
         LITERAL_SUFFIX = CASE WHEN tp.name IN ('varchar','nvarchar','char','nchar','text','ntext','datetime','datetime2','date','time','uniqueidentifier') THEN '''' ELSE NULL END, \
         CREATE_PARAMS = CASE WHEN tp.name IN ('varchar','nvarchar','char','nchar','binary','varbinary') THEN 'max length' WHEN tp.name IN ('decimal','numeric') THEN 'precision,scale' ELSE NULL END, \
         NULLABLE = CAST(1 AS SMALLINT), \
         CASE_SENSITIVE = CAST(0 AS SMALLINT), \
         SEARCHABLE = CAST(3 AS SMALLINT), \
         UNSIGNED_ATTRIBUTE = CASE WHEN tp.name IN ('tinyint') THEN CAST(1 AS SMALLINT) ELSE CAST(0 AS SMALLINT) END, \
         FIXED_PREC_SCALE = CASE WHEN tp.name IN ('money','smallmoney') THEN CAST(1 AS SMALLINT) ELSE CAST(0 AS SMALLINT) END, \
         AUTO_UNIQUE_VALUE = CAST(0 AS SMALLINT), \
         LOCAL_TYPE_NAME = tp.name, \
         MINIMUM_SCALE = CAST(0 AS SMALLINT), \
         MAXIMUM_SCALE = CASE WHEN tp.name IN ('decimal','numeric') THEN CAST(38 AS SMALLINT) WHEN tp.name IN ('datetime2','time') THEN CAST(7 AS SMALLINT) ELSE CAST(0 AS SMALLINT) END, \
         SQL_DATA_TYPE = CAST(0 AS SMALLINT), \
         SQL_DATETIME_SUB = CAST(NULL AS SMALLINT), \
         NUM_PREC_RADIX = CASE WHEN tp.name IN ('int','smallint','tinyint','bigint','decimal','numeric','money','smallmoney') THEN 10 WHEN tp.name IN ('float','real') THEN 2 ELSE NULL END, \
         INTERVAL_PRECISION = CAST(NULL AS SMALLINT) \
         FROM sys.types tp WHERE tp.system_type_id = tp.user_type_id \
         ORDER BY DATA_TYPE"
    );

    execute::exec_direct(stmt, &sql)
}

pub fn primary_keys(stmt: &mut Statement, _catalog: &str, schema: &str, table: &str) -> SQLRETURN {
    let mut conditions = vec!["1=1".to_string()];
    if !table.is_empty() {
        conditions.push(format!("t.name = N'{}'", table.replace('\'', "''")));
    }
    if !schema.is_empty() {
        conditions.push(format!("s.name = N'{}'", schema.replace('\'', "''")));
    }

    let sql = format!(
        "SELECT DB_NAME() AS TABLE_CAT, s.name AS TABLE_SCHEM, t.name AS TABLE_NAME, \
         c.name AS COLUMN_NAME, ic.key_ordinal AS KEY_SEQ, i.name AS PK_NAME \
         FROM sys.indexes i \
         JOIN sys.index_columns ic ON i.object_id = ic.object_id AND i.index_id = ic.index_id \
         JOIN sys.columns c ON ic.object_id = c.object_id AND ic.column_id = c.column_id \
         JOIN sys.tables t ON i.object_id = t.object_id \
         JOIN sys.schemas s ON t.schema_id = s.schema_id \
         WHERE i.is_primary_key = 1 AND {} \
         ORDER BY TABLE_SCHEM, TABLE_NAME, KEY_SEQ",
        conditions.join(" AND ")
    );
    execute::exec_direct(stmt, &sql)
}

pub fn statistics(
    stmt: &mut Statement,
    _catalog: &str,
    schema: &str,
    table: &str,
    unique: SQLUSMALLINT,
) -> SQLRETURN {
    let unique_filter = if unique == 0 {
        // SQL_INDEX_UNIQUE
        "AND i.is_unique = 1"
    } else {
        ""
    };

    let mut conditions = vec!["1=1".to_string()];
    if !table.is_empty() {
        conditions.push(format!("t.name = N'{}'", table.replace('\'', "''")));
    }
    if !schema.is_empty() {
        conditions.push(format!("s.name = N'{}'", schema.replace('\'', "''")));
    }

    let sql = format!(
        "SELECT DB_NAME() AS TABLE_CAT, s.name AS TABLE_SCHEM, t.name AS TABLE_NAME, \
         CASE WHEN i.is_unique = 1 THEN 0 ELSE 1 END AS NON_UNIQUE, \
         DB_NAME() AS INDEX_QUALIFIER, i.name AS INDEX_NAME, \
         CASE WHEN i.type_desc = 'CLUSTERED' THEN 1 ELSE 3 END AS TYPE, \
         ic.key_ordinal AS ORDINAL_POSITION, \
         c.name AS COLUMN_NAME, \
         CASE WHEN ic.is_descending_key = 1 THEN 'D' ELSE 'A' END AS ASC_OR_DESC, \
         CAST(NULL AS INT) AS CARDINALITY, \
         CAST(NULL AS INT) AS PAGES, \
         CAST(NULL AS VARCHAR(1)) AS FILTER_CONDITION \
         FROM sys.indexes i \
         JOIN sys.index_columns ic ON i.object_id = ic.object_id AND i.index_id = ic.index_id \
         JOIN sys.columns c ON ic.object_id = c.object_id AND ic.column_id = c.column_id \
         JOIN sys.tables t ON i.object_id = t.object_id \
         JOIN sys.schemas s ON t.schema_id = s.schema_id \
         WHERE {} {} AND i.type > 0 \
         ORDER BY NON_UNIQUE, TYPE, INDEX_NAME, ORDINAL_POSITION",
        conditions.join(" AND "),
        unique_filter
    );
    execute::exec_direct(stmt, &sql)
}

pub fn special_columns(
    stmt: &mut Statement,
    id_type: SQLUSMALLINT,
    _catalog: &str,
    schema: &str,
    table: &str,
) -> SQLRETURN {
    let mut conditions = vec!["1=1".to_string()];
    if !table.is_empty() {
        conditions.push(format!("t.name = N'{}'", table.replace('\'', "''")));
    }
    if !schema.is_empty() {
        conditions.push(format!("s.name = N'{}'", schema.replace('\'', "''")));
    }

    // SQL_BEST_ROWID = 1 (identity columns), SQL_ROWVER = 2 (timestamp/rowversion)
    let extra_filter = if id_type == 2 {
        "AND tp.name IN ('timestamp','rowversion')"
    } else {
        "AND c.is_identity = 1"
    };

    let sql = format!(
        "SELECT CAST(2 AS SMALLINT) AS SCOPE, c.name AS COLUMN_NAME, \
         CASE tp.name \
           WHEN 'int' THEN 4 WHEN 'bigint' THEN -5 WHEN 'smallint' THEN 5 \
           WHEN 'tinyint' THEN -6 WHEN 'timestamp' THEN -2 WHEN 'rowversion' THEN -2 \
           ELSE 12 END AS DATA_TYPE, \
         tp.name AS TYPE_NAME, \
         COALESCE(c.max_length, 0) AS COLUMN_SIZE, \
         COALESCE(c.max_length, 0) AS BUFFER_LENGTH, \
         c.scale AS DECIMAL_DIGITS, \
         CAST(1 AS SMALLINT) AS PSEUDO_COLUMN \
         FROM sys.columns c \
         JOIN sys.tables t ON c.object_id = t.object_id \
         JOIN sys.schemas s ON t.schema_id = s.schema_id \
         JOIN sys.types tp ON c.system_type_id = tp.system_type_id AND tp.system_type_id = tp.user_type_id \
         WHERE {} {}",
        conditions.join(" AND "),
        extra_filter
    );
    execute::exec_direct(stmt, &sql)
}

pub fn foreign_keys(
    stmt: &mut Statement,
    _pk_catalog: &str,
    pk_schema: &str,
    pk_table: &str,
    _fk_catalog: &str,
    fk_schema: &str,
    fk_table: &str,
) -> SQLRETURN {
    let mut conditions = vec!["1=1".to_string()];
    if !pk_table.is_empty() {
        conditions.push(format!("pk_t.name = N'{}'", pk_table.replace('\'', "''")));
    }
    if !pk_schema.is_empty() {
        conditions.push(format!("pk_s.name = N'{}'", pk_schema.replace('\'', "''")));
    }
    if !fk_table.is_empty() {
        conditions.push(format!("fk_t.name = N'{}'", fk_table.replace('\'', "''")));
    }
    if !fk_schema.is_empty() {
        conditions.push(format!("fk_s.name = N'{}'", fk_schema.replace('\'', "''")));
    }

    let sql = format!(
        "SELECT DB_NAME() AS PKTABLE_CAT, pk_s.name AS PKTABLE_SCHEM, pk_t.name AS PKTABLE_NAME, \
         pk_c.name AS PKCOLUMN_NAME, \
         DB_NAME() AS FKTABLE_CAT, fk_s.name AS FKTABLE_SCHEM, fk_t.name AS FKTABLE_NAME, \
         fk_c.name AS FKCOLUMN_NAME, \
         fkc.constraint_column_id AS KEY_SEQ, \
         CAST(1 AS SMALLINT) AS UPDATE_RULE, \
         CAST(1 AS SMALLINT) AS DELETE_RULE, \
         fk.name AS FK_NAME, \
         pk_i.name AS PK_NAME, \
         CAST(7 AS SMALLINT) AS DEFERRABILITY \
         FROM sys.foreign_keys fk \
         JOIN sys.foreign_key_columns fkc ON fk.object_id = fkc.constraint_object_id \
         JOIN sys.tables fk_t ON fk.parent_object_id = fk_t.object_id \
         JOIN sys.schemas fk_s ON fk_t.schema_id = fk_s.schema_id \
         JOIN sys.columns fk_c ON fkc.parent_object_id = fk_c.object_id AND fkc.parent_column_id = fk_c.column_id \
         JOIN sys.tables pk_t ON fk.referenced_object_id = pk_t.object_id \
         JOIN sys.schemas pk_s ON pk_t.schema_id = pk_s.schema_id \
         JOIN sys.columns pk_c ON fkc.referenced_object_id = pk_c.object_id AND fkc.referenced_column_id = pk_c.column_id \
         LEFT JOIN sys.indexes pk_i ON pk_t.object_id = pk_i.object_id AND pk_i.is_primary_key = 1 \
         WHERE {} \
         ORDER BY FKTABLE_CAT, FKTABLE_SCHEM, FKTABLE_NAME, KEY_SEQ",
        conditions.join(" AND ")
    );
    execute::exec_direct(stmt, &sql)
}
