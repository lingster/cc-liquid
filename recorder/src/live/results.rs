//! Persist resolved predictions as Parquet (column-compatible with the
//! Python harness's output, so the same analysis scripts read both).

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{
    ArrayRef, BooleanArray, Float32Array, Float64Array, Int64Array, UInt32Array, UInt64Array,
};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;

use crate::live::ledger::Resolved;
use crate::storage::tables::writer_props;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("horizon", DataType::UInt32, false),
        Field::new("issue_idx", DataType::UInt64, false),
        Field::new("target_idx", DataType::UInt64, false),
        Field::new("ts_issue_ms", DataType::Int64, false),
        Field::new("ts_target_ms", DataType::Int64, false),
        Field::new("p_down", DataType::Float32, false),
        Field::new("p_stationary", DataType::Float32, false),
        Field::new("p_up", DataType::Float32, false),
        Field::new("predicted", DataType::UInt32, false),
        Field::new("mid_issue", DataType::Float64, false),
        Field::new("mid_target", DataType::Float64, false),
        Field::new("move_ticks", DataType::Float64, false),
        Field::new("actual", DataType::UInt32, false),
        Field::new("correct", DataType::Boolean, false),
    ]))
}

pub fn write_results(records: &[Resolved], out: &Path) -> anyhow::Result<()> {
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
            records.iter().map(|r| r.probs[0]),
        )),
        Arc::new(Float32Array::from_iter_values(
            records.iter().map(|r| r.probs[1]),
        )),
        Arc::new(Float32Array::from_iter_values(
            records.iter().map(|r| r.probs[2]),
        )),
        Arc::new(UInt32Array::from_iter_values(
            records.iter().map(|r| r.predicted),
        )),
        Arc::new(Float64Array::from_iter_values(
            records.iter().map(|r| r.mid_issue),
        )),
        Arc::new(Float64Array::from_iter_values(
            records.iter().map(|r| r.mid_target),
        )),
        Arc::new(Float64Array::from_iter_values(
            records.iter().map(|r| r.move_ticks),
        )),
        Arc::new(UInt32Array::from_iter_values(
            records.iter().map(|r| r.actual),
        )),
        Arc::new(BooleanArray::from_iter(
            records.iter().map(|r| Some(r.correct)),
        )),
    ];
    let batch = RecordBatch::try_new(schema(), cols)?;
    let mut writer = ArrowWriter::try_new(File::create(out)?, schema(), Some(writer_props()))?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_readable_parquet() {
        let records = vec![Resolved {
            horizon: 5,
            issue_idx: 1,
            target_idx: 6,
            ts_issue_ms: 100,
            ts_target_ms: 600,
            probs: [0.2, 0.3, 0.5],
            predicted: 2,
            mid_issue: 100.0,
            mid_target: 101.0,
            move_ticks: 1.0,
            actual: 2,
            correct: true,
        }];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("results.parquet");
        write_results(&records, &path).unwrap();

        let file = File::open(&path).unwrap();
        let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap()
            .build()
            .unwrap();
        let rows: usize = reader.map(|b| b.unwrap().num_rows()).sum();
        assert_eq!(rows, 1);
    }
}
