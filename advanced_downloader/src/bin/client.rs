// WarpSpeed Downloader - src/bin/client.rs
//
// This is the entry point for the headless P2P client.
// This program's only job is to start the P2P manager in client mode
// and listen for a Master to connect to. It performs no downloading on its own.

use clap::Parser;
use std::path::Path;
use url::Url;

// Import the necessary items from our core library.
use advanced_downloader::p2p_manager::{P2PManager, PeerRole};

/// Headless P2P client for WarpSpeed Downloader.
#[derive(Parser, Debug)]
#[command(author, version, about = "A headless P2P client that helps a Master download files.")]
struct Args {
    /// URL(s) of the file to help download. Must match the URLs used by the Master
    /// so the client can identify the correct download to join.
    #[arg(short, long, required = true, num_args = 1..)]
    urls: Vec<String>,

    /// Optional output file name used by the Master. Must be provided if the Master
    /// is using a custom output path with the -o flag.
    #[arg(short, long)]
    output: Option<String>,

    /// The PIN code to connect to a protected Master.
    #[arg(long)]
    pincode: Option<String>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // The client must derive the output path and file hash in the exact same
    // way as the master to ensure they are looking for the same P2P session.
    let first_url = &args.urls[0];
    let output_path = match args.output {
        Some(path) => path,
        None => {
            let parsed_url = Url::parse(first_url).expect("Invalid URL provided.");
            Path::new(parsed_url.path())
                .file_name()
                .expect("Could not derive a filename from the URL.")
                .to_str()
                .unwrap()
                .to_string()
        }
    };

    // Corrected: Ensure hash is computed on bytes for consistency with the master.
    let file_hash = format!("{:x}", md5::compute(output_path.as_bytes()));
    println!("[Client] Starting up. Will listen for Master broadcasting for file hash: {}", file_hash);

    // The client role does not need metadata, file manager, or meta_path.
    let p2p_manager = P2PManager::new(
        PeerRole::Client,
        file_hash,
        None, // metadata
        None, // file_manager
        None, // meta_path
        args.pincode, // pincode
    )
    .await
    .expect("Failed to start P2P manager in client mode.");
    
    // Start the discovery process. This consumes the p2p_manager instance.
    p2p_manager.start();

    println!("[Client] Discovery service started. Waiting for a Master...");

    // Keep the client alive indefinitely to listen for the master.
    tokio::time::sleep(std::time::Duration::from_secs(u64::MAX)).await;
}

