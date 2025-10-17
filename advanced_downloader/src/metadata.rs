// WarpSpeed Downloader - metadata.rs

use serde::{Deserialize, Serialize};

/// Represents the state of a single chunk. Made public to be accessible by other modules.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum ChunkState {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// Represents a single chunk of the file to be downloaded. Made public.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Chunk {
    pub id: u64,
    pub start: u64,
    pub end: u64,
    pub state: ChunkState,
    // NEW: Tracks which worker (by IP or "Master-X") is currently assigned this chunk.
    pub worker_id: Option<String>,
}

/// The top-level structure that holds all metadata for a download. Made public.
/// This is the structure that will be serialized to/from the JSON `.meta` file.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DownloadMetadata {
    // CORRECTED: Changed from a single String to a Vec<String> to hold all source URLs.
    pub urls: Vec<String>,
    pub output_path: String,
    pub total_size: u64,
    pub chunks: Vec<Chunk>,
}

