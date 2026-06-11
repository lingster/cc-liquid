//! Persist resolved regression forecasts as Parquet.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float32Array, Float64Array, Int64Array, UInt32Array, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;

use crate::live_ofi::ledger::ResolvedReg;
use crate::storage::tables::writer_props;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("horizon", DataType::UInt32, false),
        Field::new("issue_idx", DataType::UInt64, false),
        Field::new("target_idx", DataType::UInt64, false),
        Field::new("ts_issue_ms", DataType::Int64, false),
        Field::new("ts_target_ms", DataType::Int64, false),
        Field::new("predicted_bps", DataType::Float32, false),
        Field::new("actual_bps", DataType::Float64, false),
        Field::new("mid_issue", DataType::Float64, false),
        Field::new("mid_target", DataType::Float64, false),
    ]))
}

pub fn write_results(records: &[ResolvedReg], out: &Path) -> anyhow::Result<()> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let cols: Vec<ArrayRef> = vec![
        Arc::new(UInt32Array::from_iter_values(
            records.iter().map(|r| r.horizon),
        )),
        Arc::new(UInt64Array::from_iter_values(
            records.iter().map(|r| r.issue_idx),
        )),
        Arc::new(UInt64Array::from_iter_values(
            records.iter().map(|r| r.target_idx),
        )),
        Arc::new(Int64Array::from_iter_values(
            records.iter().map(|r| r.ts_issue_ms),
        )),
        Arc::new(Int64Array::from_iter_values(
            records.iter().map(|r| r.ts_target_ms),
        )),
        Arc::new(Float32Array::from_iter_values(
            records.iter().map(|r| r.predicted),
        )),
        Arc::new(Float64Array::from_iter_values(
            records.iter().map(|r| r.actual),
        )),
        Arc::new(Float64Array::from_iter_values(
            records.iter().map(|r| r.mid_issue),
        )),
        Arc::new(Float64Array::from_iter_values(
            records.iter().map(|r| r.mid_target),
        )),
    ];
    let batch = RecordBatch::try_new(schema(), cols)?;
    let file = File::create(out)?;
    let mut writer = ArrowWriter::try_new(file, schema(), Some(writer_props()))?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(())
}
