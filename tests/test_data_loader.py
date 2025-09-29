from unittest.mock import MagicMock, patch

import polars as pl
import pytest
from polars.testing import assert_frame_equal

from cc_liquid.data_loader import (
    DataFrameDataSource,
    DataLoader,
    FileDataSource,
)

# Define common column names for tests
DATE_COL = "release_date"
ID_COL = "id"
PRED_COL = "pred_10d"


@pytest.fixture
def sample_dataframe():
    """Provides a sample Polars DataFrame for testing."""
    return pl.DataFrame(
        {
            DATE_COL: ["2023-01-01", "2023-01-02", "2023-01-01"],
            ID_COL: ["BTC", "ETH", "ETH"],
            PRED_COL: [0.1, -0.1, -0.05],
        }
    )


@pytest.fixture
def data_files(tmp_path, sample_dataframe):
    """Creates dummy data files (Parquet, CSV) in a temporary directory."""
    parquet_path = tmp_path / "test_data.parquet"
    csv_path = tmp_path / "test_data.csv"

    sample_dataframe.write_parquet(parquet_path)
    sample_dataframe.write_csv(csv_path)

    return {
        "parquet": str(parquet_path),
        "csv": str(csv_path),
        "unsupported": str(tmp_path / "test_data.txt"),
    }


def test_file_data_loads_parquet(data_files, sample_dataframe):
    """Test loading a Parquet file."""
    source = FileDataSource(data_files["parquet"], DATE_COL, ID_COL, PRED_COL)
    df = source.load()
    assert_frame_equal(df, sample_dataframe)


def test_file_data_loads_csv(data_files, sample_dataframe):
    """Test loading a CSV file."""
    source = FileDataSource(data_files["csv"], DATE_COL, ID_COL, PRED_COL)
    df = source.load()
    assert_frame_equal(df, sample_dataframe)


def test_file_data_raises_error_for_unsupported_type(data_files):
    """Test that an error is raised for unsupported file types."""
    source = FileDataSource(data_files["unsupported"], DATE_COL, ID_COL, PRED_COL)
    with pytest.raises(ValueError, match="Unsupported file type"):
        source.load()


def test_dataframe_data_returns_df(sample_dataframe):
    """Test that the DataFrame source returns the original DataFrame."""
    source = DataFrameDataSource(sample_dataframe.clone(), DATE_COL, ID_COL, PRED_COL)
    df = source.load()
    assert_frame_equal(df, sample_dataframe)


@patch("crowdcent_challenge.ChallengeClient")
def test_dataloader_from_crowdcent_api_happy_path(mock_challenge_client, tmp_path):
    """Test the CrowdCent API data loading flow with mocking."""
    # Arrange
    mock_api_key = "test_key"
    download_path = tmp_path / "predictions.parquet"

    # Mock the ChallengeClient instance and its download method
    mock_client_instance = MagicMock()
    mock_challenge_client.return_value = mock_client_instance

    # Create a dummy file that the 'download' will 'create'
    dummy_df = pl.DataFrame({"id": ["BTC"], "pred_10d": [0.5]})

    def fake_download(path):
        dummy_df.write_parquet(path)

    mock_client_instance.download_meta_model.side_effect = fake_download

    # Act
    df = DataLoader.from_crowdcent_api(
        api_key=mock_api_key, download_path=str(download_path)
    )

    # Assert
    mock_challenge_client.assert_called_once_with(
        challenge_slug="hyperliquid-ranking", api_key=mock_api_key
    )
    mock_client_instance.download_meta_model.assert_called_once_with(str(download_path))
    assert_frame_equal(df, dummy_df)


@patch.dict("os.environ", {"CROWDCENT_API_KEY": ""})
def test_dataloader_from_crowdcent_api_no_key():
    """Test that an error is raised if no API key is provided or found."""
    with pytest.raises(ValueError, match="CROWDCENT_API_KEY not found"):
        DataLoader.from_crowdcent_api()
