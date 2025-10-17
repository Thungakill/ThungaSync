// WarpSpeed Downloader - src/bin/main.rs
//
// This is the entry point for the main desktop application (Master or standalone).
// Its job is to parse arguments and pass them to the DownloadManager.

use clap::Parser;
use std::path::Path;
use url::Url;

// Import from our library
use advanced_downloader::download_manager::DownloadManager;

/// A blazingly fast, multi-source, resumable downloader written in Rust.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// URL(s) of the file to download.
    #[arg(short, long, required = true, num_args = 1..)]
    urls: Vec<String>,

    /// Optional output file name.
    #[arg(short, long)]
    output: Option<String>,

    /// Number of concurrent connections per source.
    #[arg(short, long, default_value_t = 8)]
    connections: u64,

    /// Run as a P2P Master, broadcasting the download and serving tasks to clients.
    #[arg(long)]
    p2p_master: bool,

    /// [Master Only] Set a PIN code required for clients to connect.
    #[arg(long, requires = "p2p_master")]
    pincode: Option<String>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let first_url = &args.urls[0];
    let output_path = match args.output {
        Some(path) => path,
        None => {
            let parsed_url = Url::parse(first_url).expect("Invalid URL.");
            Path::new(parsed_url.path())
                .file_name()
                .expect("Could not derive filename from URL.")
                .to_str().unwrap().to_string()
        }
    };
    
    // The main binary is now only a downloader or master. The client logic is separate.
    println!("Initializing download...");
    let manager = DownloadManager::new(
        args.urls,
        output_path,
        args.connections,
        args.p2p_master,
        args.pincode, // Pass the new pincode argument
    );
    
    if let Err(e) = manager.run().await {
        eprintln!("\nAn error occurred during download: {}", e);
    }
}

