//! CrowdCent meta-model integration: source the recording coin list from the
//! consolidated meta model of a CrowdCent challenge.
//!
//! Layout mirrors [`crate::info`]/[`crate::universe`]: the pure rules — parsing a
//! `.env` file and extracting the coin universe from a meta-model parquet — live
//! here and are independently testable, while the one network call (downloading
//! the parquet over authenticated HTTP) is a thin wrapper around them.
//!
//! The meta model is a long-format parquet with (at least) a date column
//! (`release_date`) and an asset-id column (`id`) whose values are Hyperliquid
//! perp coin symbols. We pick the coins for the most recent release date so the
//! recorder listens to the current prediction universe; any symbol that is not
//! actually tradeable is then dropped by [`crate::universe::validate_coins`].

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use arrow::array::{Array, StringArray};
use arrow::compute::cast;
use arrow::datatypes::DataType;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::file::reader::ChunkReader;

/// Default CrowdCent challenge whose meta model maps onto Hyperliquid perps.
pub const DEFAULT_CHALLENGE_SLUG: &str = "hyperliquid-ranking";
/// Default CrowdCent API base URL.
pub const DEFAULT_BASE_URL: &str = "https://crowdcent.com/api";
/// Default asset-id column in the meta model (values are coin symbols).
pub const DEFAULT_ID_COLUMN: &str = "id";
/// Default date column in the meta model.
pub const DEFAULT_DATE_COLUMN: &str = "release_date";
/// Environment variable holding the CrowdCent API key.
pub const API_KEY_ENV_VAR: &str = "CROWDCENT_API_KEY";

/// Upper bound on the buffered meta-model download. The real file is a few MiB;
/// this bounds memory against a hostile or malfunctioning endpoint.
const MAX_META_MODEL_BYTES: usize = 256 * 1024 * 1024;

// --- .env fallback ---------------------------------------------------------

/// Parse a `.env` file body into `(key, value)` pairs.
///
/// Pure and forgiving: blank lines and `#` comments are skipped, an optional
/// leading `export ` is stripped, the split is on the first `=`, and matching
/// surrounding single/double quotes are removed.
pub fn parse_dotenv(contents: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        out.push((key.to_string(), strip_quotes(value.trim()).to_string()));
    }
    out
}

fn strip_quotes(value: &str) -> &str {
    let bytes = value.as_bytes();
    let quoted = bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[bytes.len() - 1] == bytes[0];
    if quoted {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

/// Find the nearest `.env`, searching the current directory and its ancestors.
pub fn find_dotenv() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join(".env");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Load the nearest `.env` into the process environment as a *fallback*: a key
/// already present in the real environment always wins. No-op if no `.env` is
/// found. Call once at startup before reading any env var.
pub fn apply_dotenv_fallback() {
    let Some(path) = find_dotenv() else {
        return;
    };
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return;
    };
    for (key, value) in parse_dotenv(&contents) {
        if std::env::var_os(&key).is_none() {
            std::env::set_var(&key, value);
        }
    }
}

// --- meta-model coin extraction --------------------------------------------

/// Extract the recording coin list from a meta-model parquet.
///
/// Returns the unique `id_col` values for the latest `date_col`, preserving
/// first-seen order. Both columns are cast to UTF-8 first so the function works
/// regardless of whether the source stored them as strings, dates or integers.
pub fn coins_from_meta_model<R: ChunkReader + 'static>(
    reader: R,
    id_col: &str,
    date_col: &str,
) -> anyhow::Result<Vec<String>> {
    let batch_reader = ParquetRecordBatchReaderBuilder::try_new(reader)
        .context("opening meta-model parquet")?
        .build()?;

    let mut rows: Vec<(String, String)> = Vec::new();
    for batch in batch_reader {
        let batch = batch?;
        let ids = utf8_column(&batch, id_col)?;
        let dates = utf8_column(&batch, date_col)?;
        for i in 0..batch.num_rows() {
            if ids.is_null(i) || dates.is_null(i) {
                continue;
            }
            rows.push((dates.value(i).to_string(), ids.value(i).to_string()));
        }
    }

    if rows.is_empty() {
        bail!("meta model contained no rows");
    }

    // ISO date strings (and arrow's date/timestamp -> string casts) sort
    // lexicographically, so `max` gives the most recent release.
    let latest = rows.iter().map(|(date, _)| date).max().unwrap().clone();

    let mut seen = HashSet::new();
    let mut coins = Vec::new();
    for (date, id) in rows {
        if date == latest && seen.insert(id.clone()) {
            coins.push(id);
        }
    }
    Ok(coins)
}

/// Read `name` from `batch`, cast to a UTF-8 [`StringArray`].
fn utf8_column(
    batch: &arrow::record_batch::RecordBatch,
    name: &str,
) -> anyhow::Result<StringArray> {
    let column = batch
        .column_by_name(name)
        .ok_or_else(|| anyhow!("meta model missing column `{name}`"))?;
    let utf8 = cast(column, &DataType::Utf8)
        .with_context(|| format!("casting column `{name}` to string"))?;
    Ok(utf8
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("column `{name}` did not cast to a string array"))?
        .clone())
}

// --- network ---------------------------------------------------------------

/// Build the meta-model download URL for a challenge.
pub fn meta_model_url(base_url: &str, challenge_slug: &str) -> String {
    format!(
        "{}/challenges/{}/meta_model/download/",
        base_url.trim_end_matches('/'),
        challenge_slug
    )
}

/// Download the consolidated meta-model parquet over authenticated HTTP.
///
/// The endpoint redirects to a signed URL; `reqwest` follows redirects. The body
/// is buffered with a hard size cap.
pub async fn download_meta_model(
    base_url: &str,
    challenge_slug: &str,
    api_key: &str,
) -> anyhow::Result<Vec<u8>> {
    let url = meta_model_url(base_url, challenge_slug);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(10))
        .build()?;

    let mut resp = client
        .get(&url)
        .header("Authorization", format!("Api-Key {api_key}"))
        .send()
        .await
        .with_context(|| format!("requesting meta model from {url}"))?
        .error_for_status()
        .with_context(|| format!("meta-model download failed ({url})"))?;

    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if buf.len() + chunk.len() > MAX_META_MODEL_BYTES {
            bail!("meta model exceeded {MAX_META_MODEL_BYTES} bytes; refusing to buffer further");
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Download the meta model and extract its latest-release coin list.
pub async fn fetch_crowdcent_coins(
    base_url: &str,
    challenge_slug: &str,
    api_key: &str,
    id_col: &str,
    date_col: &str,
) -> anyhow::Result<Vec<String>> {
    let bytes = download_meta_model(base_url, challenge_slug, api_key).await?;
    coins_from_meta_model(bytes::Bytes::from(bytes), id_col, date_col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::sync::Arc;

    use arrow::array::{Date32Array, Float64Array, Int64Array, StringArray};
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;
    use parquet::arrow::ArrowWriter;

    fn write_parquet(batch: &RecordBatch) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut writer =
            ArrowWriter::try_new(file.reopen().unwrap(), batch.schema(), None).unwrap();
        writer.write(batch).unwrap();
        writer.close().unwrap();
        file
    }

    #[test]
    fn parse_dotenv_handles_comments_quotes_and_export() {
        let body = "\
# a comment
CROWDCENT_API_KEY=secret123
export FOO=\"quoted value\"
BAR='single'
EMPTY=
  SPACED = trimmed
not_a_pair
";
        let pairs = parse_dotenv(body);
        assert!(pairs.contains(&("CROWDCENT_API_KEY".into(), "secret123".into())));
        assert!(pairs.contains(&("FOO".into(), "quoted value".into())));
        assert!(pairs.contains(&("BAR".into(), "single".into())));
        assert!(pairs.contains(&("EMPTY".into(), "".into())));
        assert!(pairs.contains(&("SPACED".into(), "trimmed".into())));
        // Lines without `=` are ignored.
        assert!(!pairs.iter().any(|(k, _)| k == "not_a_pair"));
    }

    #[test]
    fn meta_model_url_trims_and_formats() {
        assert_eq!(
            meta_model_url("https://crowdcent.com/api/", "hyperliquid-ranking"),
            "https://crowdcent.com/api/challenges/hyperliquid-ranking/meta_model/download/"
        );
    }

    #[test]
    fn coins_from_meta_model_picks_latest_date_unique_in_order() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("release_date", DataType::Utf8, false),
            Field::new("id", DataType::Utf8, false),
            Field::new("pred_30d", DataType::Float64, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(vec![
                    "2024-01-01",
                    "2024-01-01",
                    "2024-01-02",
                    "2024-01-02",
                    "2024-01-02",
                ])),
                Arc::new(StringArray::from(vec!["BTC", "ETH", "SOL", "BTC", "SOL"])),
                Arc::new(Float64Array::from(vec![0.1, 0.2, 0.3, 0.4, 0.5])),
            ],
        )
        .unwrap();
        let file = write_parquet(&batch);

        let coins =
            coins_from_meta_model(File::open(file.path()).unwrap(), "id", "release_date").unwrap();
        // Latest date is 2024-01-02 -> SOL, BTC (first-seen, deduped).
        assert_eq!(coins, vec!["SOL".to_string(), "BTC".to_string()]);
    }

    #[test]
    fn coins_from_meta_model_casts_date32_and_integer_ids() {
        let schema = Arc::new(Schema::new(vec![
            // Date32 stores days since the Unix epoch.
            Field::new("release_date", DataType::Date32, false),
            Field::new("id", DataType::Int64, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Date32Array::from(vec![19723, 19724, 19724])),
                Arc::new(Int64Array::from(vec![1, 2, 3])),
            ],
        )
        .unwrap();
        let file = write_parquet(&batch);

        let coins =
            coins_from_meta_model(File::open(file.path()).unwrap(), "id", "release_date").unwrap();
        // Latest Date32 is 19724 -> integer ids 2, 3 cast to strings.
        assert_eq!(coins, vec!["2".to_string(), "3".to_string()]);
    }

    #[test]
    fn coins_from_meta_model_errors_on_missing_column() {
        let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Utf8, false)]));
        let batch =
            RecordBatch::try_new(schema, vec![Arc::new(StringArray::from(vec!["BTC"]))]).unwrap();
        let file = write_parquet(&batch);

        let err = coins_from_meta_model(File::open(file.path()).unwrap(), "id", "release_date")
            .unwrap_err();
        assert!(err.to_string().contains("release_date"));
    }
}
