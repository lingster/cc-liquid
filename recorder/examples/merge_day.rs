//! Merge an earlier same-day capture ("partial") into a session's current
//! day files, offsetting the current files' `seq` past the partial's max so
//! the merged day is one monotonic sequence in time order.
//!
//! Usage:
//! ```text
//! cargo run --release --example merge_day -- <session_dir> <partial_dir> <prefix>
//! e.g.     ... -- /data/hyperliquid/sessions/long-run \
//!                 /data/hyperliquid/sessions/long-run-20260719-partial 20260719_
//! ```
//!
//! The partial's rows come first (they are earlier in time) with their `seq`
//! unchanged; the session's rows follow with `seq += partial_max + 1`. Output
//! is written to `<file>.merged` and atomically renamed over the session file
//! only after a successful footer write. Run this only while the recorder is
//! stopped (the session files must be finalized/readable).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::UInt64Array;
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;

use hl_recorder::storage::tables::{
    max_existing_seq, writer_props, ALL_MIDS_FILE, L2_BOOK_DIR, TRADES_FILE,
};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let [_, session_dir, partial_dir, prefix] = args.as_slice() else {
        anyhow::bail!("usage: merge_day <session_dir> <partial_dir> <prefix>");
    };
    let session_dir = Path::new(session_dir);
    let partial_dir = Path::new(partial_dir);

    let offset = max_existing_seq(partial_dir, prefix)
        .ok_or_else(|| anyhow::anyhow!("no readable rows in {}", partial_dir.display()))?
        + 1;
    println!("partial max seq + 1 = {offset} (offset applied to session rows)");

    let mut pairs: Vec<(PathBuf, PathBuf)> = vec![
        (
            partial_dir.join(format!("{prefix}{ALL_MIDS_FILE}")),
            session_dir.join(format!("{prefix}{ALL_MIDS_FILE}")),
        ),
        (
            partial_dir.join(format!("{prefix}{TRADES_FILE}")),
            session_dir.join(format!("{prefix}{TRADES_FILE}")),
        ),
    ];
    let part_dir = partial_dir.join(format!("{prefix}{L2_BOOK_DIR}"));
    for entry in std::fs::read_dir(&part_dir)? {
        let p = entry?.path();
        if p.extension().is_some_and(|x| x == "parquet") {
            let name = p.file_name().unwrap();
            pairs.push((
                p.clone(),
                session_dir.join(format!("{prefix}{L2_BOOK_DIR}")).join(name),
            ));
        }
    }

    for (early, late) in pairs {
        merge_pair(&early, &late, offset)?;
    }
    println!("done");
    Ok(())
}

/// Write `early` rows verbatim, then `late` rows with `seq += offset`, into
/// `late`.merged, then rename over `late`.
fn merge_pair(early: &Path, late: &Path, offset: u64) -> anyhow::Result<()> {
    anyhow::ensure!(early.exists(), "missing {}", early.display());
    anyhow::ensure!(late.exists(), "missing {}", late.display());

    let late_reader = ParquetRecordBatchReaderBuilder::try_new(File::open(late)?)?;
    let schema = late_reader.schema().clone();
    let seq_idx = schema.index_of("seq")?;

    let tmp = late.with_extension("parquet.merged");
    let mut writer = ArrowWriter::try_new(File::create(&tmp)?, schema, Some(writer_props()))?;

    let mut early_rows = 0usize;
    for batch in ParquetRecordBatchReaderBuilder::try_new(File::open(early)?)?.build()? {
        let batch = batch?;
        early_rows += batch.num_rows();
        writer.write(&batch)?;
    }
    let mut late_rows = 0usize;
    for batch in late_reader.build()? {
        let batch = batch?;
        late_rows += batch.num_rows();
        writer.write(&offset_seq(&batch, seq_idx, offset)?)?;
    }
    writer.close()?;
    std::fs::rename(&tmp, late)?;
    println!(
        "{}: {early_rows} early + {late_rows} late rows merged",
        late.display()
    );
    Ok(())
}

/// Rebuild a batch with `offset` added to its `seq` column.
fn offset_seq(batch: &RecordBatch, seq_idx: usize, offset: u64) -> anyhow::Result<RecordBatch> {
    let seq = batch
        .column(seq_idx)
        .as_any()
        .downcast_ref::<UInt64Array>()
        .ok_or_else(|| anyhow::anyhow!("seq column is not u64"))?;
    let shifted: UInt64Array = seq.values().iter().map(|v| v + offset).collect();
    let mut cols = batch.columns().to_vec();
    cols[seq_idx] = Arc::new(shifted);
    Ok(RecordBatch::try_new(batch.schema(), cols)?)
}
