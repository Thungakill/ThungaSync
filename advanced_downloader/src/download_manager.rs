// WarpSpeed Downloader - download_manager.rs

use futures::future::join_all;
use indicatif::ProgressBar;
use itertools::Itertools;
use reqwest::{header, Client};
use std::cmp::min;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::time::sleep;

// Import our own modules.
use crate::chunk_downloader::{ChunkDownloader, DownloadError};
use crate::file_manager::FileManager;
use crate::metadata::{Chunk, ChunkState, DownloadMetadata};
use crate::p2p_manager::{P2PManager, PeerRole, WorkerState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    pub url: String,
    pub size: u64,
    pub etag: Option<String>,
}

#[derive(Error, Debug)]
pub enum ManagerError {
    #[error("Network request failed: {0}")]
    Network(#[from] reqwest::Error),
    #[error("File I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Chunk download failed: {0}")]
    Chunk(#[from] DownloadError),
    #[error("Server does not support range requests: {0}")]
    RangeRequestsNotSupported(String),
    #[error("Could not get content length from server: {0}")]
    NoContentLength(String),
    #[error("Source mismatch: File at {url} has a different size. Expected {expected_size}, found {found_size}.")]
    SourceMismatch {
        url: String,
        expected_size: u64,
        found_size: u64,
    },
    #[error("No valid sources found.")]
    NoValidSources,
    #[error("Resume error: File size on server ({server_size}) does not match metadata ({meta_size}).")]
    ResumeValidationError {
        server_size: u64,
        meta_size: u64,
    },
}

pub struct DownloadManager {
    urls: Vec<String>,
    output_path: String,
    num_chunks: u64,
    client: Client,
    p2p_master_enabled: bool,
    pincode: Option<String>, // <-- MODIFIED: Added field for PIN code
}

impl DownloadManager {
    // MODIFIED: Added 'pincode' parameter to the constructor
    pub fn new(urls: Vec<String>, output_path: String, num_chunks: u64, p2p_master_enabled: bool, pincode: Option<String>) -> Self {
        Self {
            urls,
            output_path,
            num_chunks,
            client: Client::new(),
            p2p_master_enabled,
            pincode, // <-- MODIFIED: Store the PIN code
        }
    }

    pub async fn run(&self) -> Result<(), ManagerError> {
        const MAX_RETRIES: u32 = 3;
        const RETRY_DELAY: Duration = Duration::from_secs(2);

        let meta_path = self.get_meta_path();
        let metadata: DownloadMetadata;

        if Path::new(&meta_path).exists() {
            println!("Resuming download...");
            let mut loaded_meta = self.load_metadata(&meta_path)?;

            // FIX: Smarter resume logic
            let mut changed = false;
            for chunk in loaded_meta.chunks.iter_mut() {
                if chunk.state == ChunkState::InProgress {
                    if let Some(worker_id) = &chunk.worker_id {
                        if worker_id.starts_with("Master-") {
                            chunk.state = ChunkState::Pending;
                            chunk.worker_id = None;
                            changed = true;
                        }
                    } else {
                        chunk.state = ChunkState::Pending;
                        changed = true;
                    }
                }
            }

            if changed {
                self.save_metadata(&meta_path, &loaded_meta)?;
            }


            let verified_sources = self.verify_sources().await?;
            let total_size_on_server = verified_sources[0].size;

            if loaded_meta.total_size != total_size_on_server {
                return Err(ManagerError::ResumeValidationError {
                    server_size: total_size_on_server,
                    meta_size: loaded_meta.total_size,
                });
            }
            loaded_meta.urls = verified_sources.into_iter().map(|s| s.url).collect();
            metadata = loaded_meta;
        } else {
            println!("Starting new download...");
            let verified_sources = self.verify_sources().await?;
            metadata = self.create_metadata(
                verified_sources[0].size,
                verified_sources.into_iter().map(|s| s.url).collect(),
            );
            self.save_metadata(&meta_path, &metadata)?;
        }

        let file_manager = FileManager::new(&self.output_path, metadata.total_size).await?;
        let file_manager_arc = Arc::new(Mutex::new(file_manager));
        let metadata_arc = Arc::new(Mutex::new(metadata.clone()));
        
        let workers_map_clone = if self.p2p_master_enabled {
            let file_hash = format!("{:x}", md5::compute(self.output_path.as_bytes()));
            let p2p_manager = P2PManager::new(
                PeerRole::Master,
                file_hash,
                Some(Arc::clone(&metadata_arc)),
                Some(Arc::clone(&file_manager_arc)),
                Some(meta_path.clone()),
                self.pincode.clone(), // <-- MODIFIED: Pass the PIN code to the P2PManager
            )
            .await?;
            let workers = Some(Arc::clone(&p2p_manager.workers));
            p2p_manager.start();
            workers
        } else {
            None
        };
        
        let find_chunk_for_master = |meta: &mut DownloadMetadata, worker_id: &str| -> Option<Chunk> {
            if let Some(chunk_to_do) = meta.chunks.iter_mut().find(|c| c.state == ChunkState::Pending) {
                chunk_to_do.state = ChunkState::InProgress;
                chunk_to_do.worker_id = Some(worker_id.to_string());
                return Some(chunk_to_do.clone());
            }
            None
        };
        
        let num_master_workers = 4;
        let mut tasks = vec![];

        for i in 0..num_master_workers {
            let worker_id = format!("Master-{}", i);
            let metadata_clone = Arc::clone(&metadata_arc);
            let file_manager_clone = Arc::clone(&file_manager_arc);
            let downloaders: Vec<_> = metadata_arc.lock().await.urls.iter().map(|url| ChunkDownloader::new(url)).collect();
            let workers_clone_for_task = workers_map_clone.clone();
            let meta_path_clone = meta_path.clone();

            let task = tokio::spawn(async move {
                loop {
                    let chunk_to_download = {
                        let mut meta = metadata_clone.lock().await;
                        find_chunk_for_master(&mut meta, &worker_id)
                    };

                    if let Some(chunk) = chunk_to_download {
                        let pb = ProgressBar::new(chunk.end - chunk.start + 1);

                        if let Some(workers) = &workers_clone_for_task {
                            let mut guard = workers.lock().await;
                            guard.entry(worker_id.clone()).or_insert_with(|| WorkerState {
                                id: worker_id.clone(),
                                current_chunk: Some(chunk.id),
                                progress: 0,
                            });
                             if let Some(worker) = guard.get_mut(&worker_id) {
                                worker.current_chunk = Some(chunk.id);
                                worker.progress = 0;
                            }
                        }

                        let workers_clone_for_progress = workers_clone_for_task.clone();
                        let pb_clone = pb.clone();
                        let worker_id_for_updater = worker_id.clone();
                        let progress_updater = tokio::spawn(async move {
                            if let Some(workers) = workers_clone_for_progress {
                                loop {
                                    tokio::time::sleep(Duration::from_millis(500)).await;
                                    let progress = pb_clone.position();
                                    let mut guard = workers.lock().await;
                                    if let Some(worker) = guard.get_mut(&worker_id_for_updater) {
                                        if worker.current_chunk == Some(chunk.id) {
                                            worker.progress = progress;
                                        } else { break; }
                                    } else { break; }
                                     if progress >= pb_clone.length().unwrap_or(0) { break; }
                                }
                            }
                        });
                        
                        let downloader = downloaders[chunk.id as usize % downloaders.len()].clone();
                        let mut chunk_is_done = false;

                        for _ in 0..MAX_RETRIES {
                             match downloader.download_chunk(chunk.start, chunk.end, &pb).await {
                                Ok(data) => {
                                    file_manager_clone.lock().await.write_chunk(chunk.start, &data).await.unwrap();
                                    let mut meta = metadata_clone.lock().await;
                                    meta.chunks[chunk.id as usize].state = ChunkState::Completed;
                                    Self::save_metadata_static(&meta_path_clone, &*meta).unwrap();
                                    chunk_is_done = true;
                                    break;
                                }
                                Err(_) => {
                                    sleep(RETRY_DELAY).await;
                                }
                            }
                        }
                        
                        progress_updater.await.ok();

                        if !chunk_is_done {
                            let mut meta = metadata_clone.lock().await;
                            if let Some(c) = meta.chunks.get_mut(chunk.id as usize) {
                                if c.worker_id.as_deref() == Some(&worker_id) {
                                    c.state = ChunkState::Pending;
                                    c.worker_id = None;
                                    Self::save_metadata_static(&meta_path_clone, &*meta).unwrap();
                                }
                            }
                        }
                    } else {
                        break;
                    }
                }
                if let Some(workers) = &workers_clone_for_task {
                    let mut guard = workers.lock().await;
                    if let Some(worker) = guard.get_mut(&worker_id) {
                        worker.current_chunk = None;
                        worker.progress = 0;
                    }
                }
            });
            tasks.push(task);
        }

        join_all(tasks).await;
        Ok(())
    }

    async fn verify_sources(&self) -> Result<Vec<SourceInfo>, ManagerError> {
        let verification_tasks: Vec<_> = self.urls.iter().map(|url| self.get_source_info(url)).collect();
        let results = join_all(verification_tasks).await;

        let mut verified_sources = Vec::new();
        for result in results {
            if let Ok(info) = result {
                verified_sources.push(info);
            }
        }
        if verified_sources.is_empty() {
            return Err(ManagerError::NoValidSources);
        }
        if !verified_sources.iter().map(|s| s.size).all_equal() {
            let first = &verified_sources[0];
            if let Some(mismatch) = verified_sources.iter().find(|&info| info.size != first.size) {
                return Err(ManagerError::SourceMismatch {
                    url: mismatch.url.clone(),
                    expected_size: first.size,
                    found_size: mismatch.size,
                });
            }
        }
        Ok(verified_sources)
    }

    fn create_metadata(&self, total_size: u64, urls: Vec<String>) -> DownloadMetadata {
        let mut chunks = Vec::new();
        let chunk_size = (total_size as f64 / self.num_chunks as f64).ceil() as u64;
        let mut start = 0;

        for i in 0..self.num_chunks {
            let end = min(start + chunk_size - 1, total_size - 1);
            chunks.push(Chunk {
                id: i,
                start,
                end,
                state: ChunkState::Pending,
                worker_id: None,
            });
            start = end + 1;
            if start >= total_size {
                break;
            }
        }

        DownloadMetadata {
            urls,
            output_path: self.output_path.clone(),
            total_size,
            chunks,
        }
    }

    async fn get_source_info(&self, url: &str) -> Result<SourceInfo, ManagerError> {
        let response = self.client.head(url).send().await?;
        if !response.status().is_success() {
            return Err(ManagerError::Network(response.error_for_status().unwrap_err()));
        }

        if !response.headers().get(header::ACCEPT_RANGES).map_or(false, |val| val == "bytes") {
            return Err(ManagerError::RangeRequestsNotSupported(url.to_string()));
        }

        let size = response
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|val| val.to_str().ok())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| ManagerError::NoContentLength(url.to_string()))?;

        let etag = response
            .headers()
            .get(header::ETAG)
            .and_then(|val| val.to_str().ok())
            .map(|s| s.to_string());

        Ok(SourceInfo {
            url: url.to_string(),
            size,
            etag,
        })
    }

    fn get_meta_path(&self) -> String {
        format!("{}.meta", self.output_path)
    }

    fn save_metadata(&self, path: &str, metadata: &DownloadMetadata) -> Result<(), ManagerError> {
        let json = serde_json::to_string_pretty(metadata)?;
        fs::write(path, json)?;
        Ok(())
    }

    pub fn save_metadata_static(path: &str, metadata: &DownloadMetadata) -> Result<(), ManagerError> {
        let json = serde_json::to_string_pretty(metadata)?;
        fs::write(path, json)?;
        Ok(())
    }

    fn load_metadata(&self, path: &str) -> Result<DownloadMetadata, ManagerError> {
        let json = fs::read_to_string(path)?;
        let metadata = serde_json::from_str(&json)?;
        Ok(metadata)
    }
}
