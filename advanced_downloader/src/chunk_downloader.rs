// WarpSpeed Downloader - chunk_downloader.rs

use indicatif::ProgressBar;
use reqwest::{Client, StatusCode};
use thiserror::Error;

/// Custom error types for the download process.
#[derive(Error, Debug)]
pub enum DownloadError {
    #[error("Network request failed: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Server returned an unsuccessful status code: {0}")]
    Unsuccessful(StatusCode),
}

/// A downloader for a single chunk.
#[derive(Clone)]
pub struct ChunkDownloader {
    client: Client,
    url: String,
}

impl ChunkDownloader {
    pub fn new(url: &str) -> Self {
        ChunkDownloader {
            client: Client::new(),
            url: url.to_string(),
        }
    }

    /// Downloads a specific byte range, updating a progress bar as it goes.
    ///
    /// This function is used by both the Master's local workers and the remote P2P Clients.
    /// The `ProgressBar` instance it receives is polled by the calling context:
    ///   - In `download_manager.rs`, a separate task polls the bar to update the master's UI.
    ///   - In `client.rs`, a separate task will poll the bar and send the progress
    ///     to the master via an HTTP request.
    pub async fn download_chunk(
        &self,
        start: u64,
        end: u64,
        pb: &ProgressBar, // Accept a progress bar to report progress on.
    ) -> Result<Vec<u8>, DownloadError> {
        let range_header = format!("bytes={}-{}", start, end);

        let mut response = self.client.get(&self.url)
            .header("Range", range_header)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(DownloadError::Unsuccessful(response.status()));
        }

        let total_size = end - start + 1;
        let mut downloaded_bytes = Vec::with_capacity(total_size as usize);

        // This loop reads the next available chunk of bytes from the download stream.
        while let Some(chunk) = response.chunk().await? {
            downloaded_bytes.extend_from_slice(&chunk);
            // Increment the progress bar by the number of bytes received.
            // This is the key mechanism for reporting progress back to the caller.
            pb.inc(chunk.len() as u64);
        }

        Ok(downloaded_bytes)
    }
}
