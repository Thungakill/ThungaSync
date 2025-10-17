use axum::{
    extract::{
        ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade},
        ConnectInfo, Path, State,
    },
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Json},
    routing::{get, post},
    Router,
};
use bytes::Bytes;
use futures_util::{sink::SinkExt, stream::StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use rand::{seq::SliceRandom, Rng};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use socket2::{Domain, Socket, Type};
use std::collections::HashMap;
use std::env;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio::time::interval;
use tokio_stream::wrappers::IntervalStream;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message as WsMessage};
use url::Url;

use crate::download_manager::DownloadManager;
use crate::file_manager::FileManager;
use crate::metadata::{Chunk, ChunkState, DownloadMetadata};

const DISCOVERY_PORT: u16 = 34254;
const BROADCAST_ADDRESS: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 255);
const HTTP_PORT: u16 = 8080;
const MB: f32 = 1024.0 * 1024.0;

const SIGNALING_SERVER_URL: &str = "wss://your-thungasync-url.pages.dev/ws"; // IMPORTANT: Replace with your actual Cloudflare URL later

const ADJECTIVES: &[&str] = &["BLUE", "RED", "QUICK", "HAPPY", "BRIGHT", "SILENT", "FAST", "COOL", "WARM", "FRESH"];
const NOUNS: &[&str] = &["FOX", "DOG", "CAT", "CLOUD", "STAR", "RIVER", "LEAF", "STONE", "WAVE", "PINE"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerRole {
    Master,
    Client,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum P2PMessage {
    Discovery {
        file_hash: String,
        master_ip: Ipv4Addr,
        http_port: u16,
        urls: Vec<String>,
        pin_required: bool,
    },
    ClientReady {
        client_ip: Ipv4Addr,
        http_port: u16,
    },
}
#[derive(Serialize)]
struct RegistrationMessage<'a> {
    r#type: &'a str,
    code: &'a str,
    #[serde(rename = "localIp")] 
    local_ip: &'a str,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProgressUpdate {
    pub bytes_downloaded: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkerState {
    pub id: String,
    pub current_chunk: Option<u64>,
    pub progress: u64,
}

#[derive(Serialize, Clone)]
pub struct StatusPayload {
    metadata: DownloadMetadata,
    workers: HashMap<String, WorkerState>,
}

#[derive(Debug)]
enum DownloadPieceError {
    Network,
    MasterOffline,
}

#[derive(Clone)]
pub struct ApiState {
    pub metadata: Arc<Mutex<DownloadMetadata>>,
    pub file_manager: Arc<Mutex<FileManager>>,
    pub meta_path: Arc<String>,
    pub workers: Arc<Mutex<HashMap<String, WorkerState>>>,
    pub pin_code: Option<String>,
}

pub struct P2PManager {
    role: PeerRole,
    file_hash: String,
    socket: Arc<UdpSocket>,
    pub workers: Arc<Mutex<HashMap<String, WorkerState>>>,
    api_state: Option<ApiState>,
    pin_code: Option<String>,
    mdns_daemon: Arc<ServiceDaemon>,
}

fn generate_code() -> String {
    let mut rng = rand::thread_rng();
    let adjective = ADJECTIVES.choose(&mut rng).unwrap_or(&"FAST");
    let noun = NOUNS.choose(&mut rng).unwrap_or(&"SYNC");
    let number: u16 = rng.gen_range(1..1000);
    format!("{}-{}-{}", adjective, noun, number)
}

impl P2PManager {
    pub async fn new(
        role: PeerRole,
        file_hash: String,
        metadata: Option<Arc<Mutex<DownloadMetadata>>>,
        file_manager: Option<Arc<Mutex<FileManager>>>,
        meta_path: Option<String>,
        pin_code: Option<String>,
    ) -> std::io::Result<Self> {
        let listen_address: SocketAddr = "0.0.0.0:34254".parse().unwrap();
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, None)?;
        socket.set_reuse_address(true)?;
        socket.bind(&listen_address.into())?;
        socket.set_broadcast(true)?;
        let socket = UdpSocket::from_std(socket.into())?;
        let mdns_daemon = Arc::new(ServiceDaemon::new().expect("Failed to create mDNS daemon."));
        let workers = Arc::new(Mutex::new(HashMap::new()));
        let api_state = if role == PeerRole::Master {
            Some(ApiState {
                metadata: metadata.expect("Master must have metadata"),
                file_manager: file_manager.expect("Master must have a file manager"),
                meta_path: Arc::new(meta_path.expect("Master must have a meta_path")),
                workers: Arc::clone(&workers),
                pin_code: pin_code.clone(),
            })
        } else {
            None
        };
        Ok(Self {
            role,
            file_hash,
            socket: Arc::new(socket),
            workers,
            api_state,
            pin_code,
            mdns_daemon,
        })
    }

    pub fn start(self) {
        let socket_clone = Arc::clone(&self.socket);
        let role = self.role;
        let file_hash = self.file_hash.clone();
        let api_state_clone = self.api_state.clone();
        let workers_clone_for_ui = Arc::clone(&self.workers);
        let metadata_clone_for_ui = api_state_clone.as_ref().map(|s| Arc::clone(&s.metadata));
        let mdns_daemon_clone = Arc::clone(&self.mdns_daemon);
        let pin_required = self.pin_code.is_some();

        tokio::spawn(async move {
            match role {
                PeerRole::Master => {
                   if let Ok(my_local_ip) = local_ip_address::local_ip() {
    let session_code = generate_code();
    let local_ip_str = my_local_ip.to_string();
    let session_code_clone = session_code.clone();

                        tokio::spawn(async move {
        let url = Url::parse(SIGNALING_SERVER_URL).unwrap();
        match connect_async(url).await {
            Ok((ws_stream, _)) => {
                println!("[Signaling] Connected to broker.");
                let (mut write, _) = ws_stream.split();
                // Use the cloned code here
                let reg_msg = RegistrationMessage {
                    r#type: "register",
                    code: &session_code_clone, 
                    local_ip: &local_ip_str, // Use the corrected field name
                };
                                   let json_msg = serde_json::to_string(&reg_msg).unwrap();
                if write.send(WsMessage::Text(json_msg)).await.is_ok() {
                    println!("[Signaling] Successfully registered session code: {}", session_code_clone);
                } else {
                    eprintln!("[Signaling] ERROR: Failed to send registration message to broker.");
                }
                                }
                                Err(e) => {
                                    eprintln!("[Signaling] ERROR: Could not connect to the signaling server at {}: {}", SIGNALING_SERVER_URL, e);
                                    eprintln!("[Signaling] Public connection via code will not work.");
                                }
                            }
                        });
                        
                        println!("\n=======================================================");
                        println!(" Your Public Connection Code is: {}", session_code);
                        println!(" Enter this code at the ThungaSync Hub website.");
                        println!("=======================================================\n");

                        let server_state = api_state_clone.clone().unwrap();
                        tokio::spawn(async move {
                            Self::start_master_server(server_state).await;
                        });

                        let service_type = format!("_thungasync._tcp.local.");
                        let instance_name = format!("ThungaSync-Master-{}", &file_hash[..6]);
                        let host_name = format!("{}.local.", hostname::get().unwrap().to_string_lossy());

                        let service_info = ServiceInfo::new(&service_type, &instance_name, &host_name, &my_local_ip.to_string(), HTTP_PORT, None)
                            .unwrap()
                            .enable_addr_auto();
                        mdns_daemon_clone.register(service_info).expect("Failed to register mDNS service.");

                        let broadcast_socket = socket_clone.clone();
                        tokio::spawn(async move {
                            Self::broadcast_presence(broadcast_socket, file_hash, api_state_clone.unwrap(), pin_required).await;
                        });

                        if let Some(metadata) = metadata_clone_for_ui {
                            tokio::spawn(async move {
                                Self::render_mission_control_ui(workers_clone_for_ui, metadata).await;
                            });
                        }
                    } else {
                        eprintln!("Could not determine local IP address. Public and local sharing will not work.");
                    }
                }
                PeerRole::Client => {
                    println!("[P2P] Operating as Client. Listening for a Master...");
                    Self::listen_for_master(socket_clone, file_hash, mdns_daemon_clone, self.pin_code).await;
                }
            }
        });
    }

    async fn start_master_server(api_state: ApiState) {
        let app = Router::new()
            .route("/", get(serve_html))
            .route("/ws", get(websocket_handler))
            .route("/get-task", get(get_task))
            .route("/submit-chunk/:id", post(submit_chunk))
            .route("/update-progress/:id", post(update_progress))
            .route("/heartbeat", get(heartbeat))
            .route("/status", get(get_status));
        let addr = SocketAddr::from(([0, 0, 0, 0], HTTP_PORT));
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        axum::serve(listener, app.with_state(api_state).into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap();
    }

    async fn broadcast_presence(socket: Arc<UdpSocket>, file_hash: String, api_state: ApiState, pin_required: bool) {
        let my_local_ip = local_ip_address::local_ip().expect("Failed to get local IP");
        let my_ip_v4 = match my_local_ip {
            std::net::IpAddr::V4(ip) => ip,
            _ => panic!("IPv6 is not supported."),
        };
        let urls = api_state.metadata.lock().await.urls.clone();
        let message = P2PMessage::Discovery {
            file_hash,
            master_ip: my_ip_v4,
            http_port: HTTP_PORT,
            urls,
            pin_required,
        };
        let message_bytes = serde_json::to_vec(&message).unwrap();
        let broadcast_socket_addr = SocketAddrV4::new(BROADCAST_ADDRESS, DISCOVERY_PORT);
        loop {
            socket.send_to(&message_bytes, &broadcast_socket_addr).await.ok();
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    async fn render_mission_control_ui(workers: Arc<Mutex<HashMap<String, WorkerState>>>, metadata: Arc<Mutex<DownloadMetadata>>) {
        let main_pb = ProgressBar::new(100);
        main_pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {percent}% ({msg})")
                .unwrap()
                .progress_chars("=> "),
        );
        loop {
            let workers_guard = workers.lock().await;
            let metadata_guard = metadata.lock().await;
            print!("\x1B[2J\x1B[1;1H");
            let completed_chunks = metadata_guard.chunks.iter().filter(|c| c.state == ChunkState::Completed).count();
            let total_chunks = metadata_guard.chunks.len();
            let progress_percent = if total_chunks > 0 { (completed_chunks as f32 / total_chunks as f32) * 100.0 } else { 100.0 };
            main_pb.set_position(progress_percent as u64);
            main_pb.set_message(format!("{}/{} Chunks", completed_chunks, total_chunks));
            println!("--- ThungaSync Downloader: Mission Control ---");
            main_pb.println("");
            let master_workers: Vec<_> = workers_guard.iter().filter(|(id, _)| id.starts_with("Master-")).collect();
            let peer_workers: Vec<_> = workers_guard.iter().filter(|(id, _)| !id.starts_with("Master-")).collect();
            if !master_workers.is_empty() {
                println!("--- Master Device (Your PC) ---");
                for (_, worker) in master_workers {
                    if let Some(chunk_id) = worker.current_chunk {
                        if let Some(chunk) = metadata_guard.chunks.get(chunk_id as usize) {
                            let chunk_size = (chunk.end - chunk.start + 1) as f64;
                            let progress_pct = (worker.progress as f64 / chunk_size) * 100.0;
                            let bar = render_progress_bar(progress_pct);
                            println!("  Chunk #{:<2} [{:<27}] {:>5.2}/{:<5.2} MB", chunk.id, bar, worker.progress as f32 / MB, chunk_size as f32 / MB);
                        }
                    }
                }
            }
            if !peer_workers.is_empty() {
                println!("\n--- Peer Devices ---");
                let mut peers: HashMap<String, Vec<&WorkerState>> = HashMap::new();
                for (id, worker) in peer_workers {
                    peers.entry(id.to_string()).or_default().push(worker);
                }
                for (id, workers_for_peer) in peers {
                    println!("--- Peer Device ({}) ---", id);
                    for worker in workers_for_peer {
                        if let Some(chunk_id) = worker.current_chunk {
                            if let Some(chunk) = metadata_guard.chunks.get(chunk_id as usize) {
                                let chunk_size = (chunk.end - chunk.start + 1) as f64;
                                if chunk.state == ChunkState::Completed {
                                    println!("  Chunk #{:<2} [===========================] {:>5.2}/{:<5.2} MB (Completed)", chunk.id, chunk_size as f32 / MB, chunk_size as f32 / MB);
                                } else {
                                    let progress_pct = (worker.progress as f64 / chunk_size) * 100.0;
                                    let bar = render_progress_bar(progress_pct);
                                    println!("  Chunk #{:<2} [{:<27}] {:>5.2}/{:<5.2} MB", chunk.id, bar, worker.progress as f32 / MB, chunk_size as f32 / MB);
                                }
                            }
                        }
                    }
                }
            }
            let pending_chunks: Vec<_> = metadata_guard.chunks.iter().filter(|c| c.state == ChunkState::Pending).map(|c| c.id).collect();
            let completed_chunks_list: Vec<_> = metadata_guard.chunks.iter().filter(|c| c.state == ChunkState::Completed).map(|c| c.id).collect();
            println!("\n--- Download Queue ---");
            println!("Pending: {} chunks", pending_chunks.len());
            println!("Completed: {} chunks", completed_chunks_list.len());
            drop(workers_guard);
            drop(metadata_guard);
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    async fn listen_for_master(socket: Arc<UdpSocket>, file_hash_to_find: String, mdns_daemon: Arc<ServiceDaemon>, pin_code: Option<String>) {
        let service_type = format!("_thungasync._tcp.local.");
        let receiver = mdns_daemon.browse(&service_type).expect("Failed to browse mDNS services.");
        let mut buf = vec![0u8; 1024];
        loop {
            tokio::select! {
                Ok(service_event) = receiver.recv_async() => {
                    if let mdns_sd::ServiceEvent::ServiceResolved(info) = service_event {
                        let instance_name = format!("ThungaSync-Master-{}", &file_hash_to_find[..6]);
                        if info.get_fullname().starts_with(&instance_name) {
                            if let Some(master_ip) = info.get_addresses().iter().find(|a| a.is_ipv4()) {
                                let master_port = info.get_port();
                                let master_api_addr = format!("http://{}:{}", master_ip, master_port);
                                println!("[mDNS] Found Master at {}. The client will now switch to UDP to get the file URLs. If it doesn't connect, ensure the Master is still broadcasting.", master_api_addr);
                            }
                        }
                    }
                },
                Ok((size, _)) = socket.recv_from(&mut buf) => {
                    if let Ok(P2PMessage::Discovery { file_hash, master_ip, http_port, urls, pin_required }) = serde_json::from_slice(&buf[..size]) {
                        if file_hash == file_hash_to_find {
                            if pin_required && pin_code.is_none() {
                                eprintln!("[P2P Client] Master requires a PIN, but none was provided. Exiting.");
                                break;
                            }
                            let master_api_addr = format!("http://{}:{}", master_ip, http_port);
                            println!("[UDP] Found Master at {}. Starting download work...", master_api_addr);
                            Self::client_work_loop(master_api_addr, urls, pin_code.clone()).await;
                            println!("[P2P Client] Work finished or Master is offline. Exiting.");
                            break;
                        }
                    }
                }
            }
        }
    }

    async fn client_work_loop(master_api_addr: String, urls: Vec<String>, pin_code: Option<String>) {
        let http_client = Client::new();
        loop {
            if http_client.get(format!("{}/heartbeat", master_api_addr)).send().await.is_err() {
                eprintln!("[P2P Client] Master appears to be offline. Shutting down.");
                break;
            }
            let task_url = format!("{}/get-task", master_api_addr);
            let mut request_builder = http_client.get(&task_url);
            if let Some(pin) = &pin_code {
                request_builder = request_builder.header("X-ThungaSync-PIN", pin);
            }
            let response = match request_builder.send().await {
                Ok(res) => res,
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
            if response.status() == StatusCode::UNAUTHORIZED {
                eprintln!("[P2P Client] Unauthorized. The provided PIN may be incorrect. Exiting.");
                break;
            }
            if response.status() == StatusCode::NO_CONTENT {
                println!("[P2P Client] No more tasks available. Work is done.");
                break;
            }
            if response.status() != StatusCode::OK {
                eprintln!("[P2P Client] Master returned an error status: {}. Retrying...", response.status());
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            let chunk: Chunk = match response.json().await {
                Ok(c) => c,
                Err(_) => continue,
            };
            println!("[P2P Client] Got task: Downloading Chunk #{}", chunk.id);
            let download_result = Self::download_chunk_concurrently(&http_client, &master_api_addr, &urls[0], &chunk).await;
            match download_result {
                Ok(data) => {
                    let submit_url = format!("{}/submit-chunk/{}", master_api_addr, chunk.id);
                    if http_client.post(&submit_url).body(data).send().await.is_err() {
                        eprintln!("[P2P Client] Failed to submit chunk #{}", chunk.id);
                    } else {
                        println!("[P2P Client] Successfully submitted chunk #{}", chunk.id);
                    }
                }
                Err(e) => {
                    eprintln!("[P2P Client] Failed to download chunk #{}: {:?}", chunk.id, e);
                }
            }
        }
    }

    async fn download_chunk_concurrently(client: &Client, master_addr: &str, url: &str, chunk: &Chunk) -> Result<Vec<u8>, DownloadPieceError> {
        let range_header = format!("bytes={}-{}", chunk.start, chunk.end);
        let response_stream = match client.get(url).header("Range", range_header).send().await {
            Ok(res) => res.bytes_stream(),
            Err(_) => return Err(DownloadPieceError::Network),
        };
        tokio::pin!(response_stream);
        let total_size = chunk.end - chunk.start + 1;
        let mut downloaded_bytes = Vec::with_capacity(total_size as usize);
        let mut heartbeat = interval(Duration::from_secs(2));
        let mut final_result: Result<(), DownloadPieceError> = Ok(());
        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    if client.get(format!("{}/heartbeat", master_addr)).send().await.is_err() {
                        eprintln!("[P2P Client] Heartbeat failed. Master is offline.");
                        final_result = Err(DownloadPieceError::MasterOffline);
                        break;
                    }
                }
                item = response_stream.next() => {
                    match item {
                        Some(Ok(piece)) => {
                            downloaded_bytes.extend_from_slice(&piece);
                            let progress = ProgressUpdate { bytes_downloaded: downloaded_bytes.len() as u64 };
                            client.post(format!("{}/update-progress/{}", master_addr, chunk.id))
                                .json(&progress)
                                .send().await.ok();
                        },
                        Some(Err(_)) => {
                            final_result = Err(DownloadPieceError::Network);
                            break;
                        },
                        None => break,
                    }
                }
            }
        }
        final_result.map(|_| downloaded_bytes)
    }
}

fn render_progress_bar(percentage: f64) -> String {
    let bar_width = 27;
    let filled_width = (percentage / 100.0 * bar_width as f64).round() as usize;
    let empty_width = bar_width - filled_width;
    format!("{}{}", "=".repeat(filled_width), "-".repeat(empty_width))
}

async fn serve_html() -> impl IntoResponse {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = std::path::Path::new(manifest_dir).join("frontend").join("index.html");
    println!("[DEBUG] HTTP request received. Attempting to serve file from absolute path: '{:?}'", path);
    match tokio::fs::read_to_string(&path).await {
        Ok(html) => {
            println!("[DEBUG] SUCCESS: Successfully read index.html ({} bytes). Sending to browser.", html.len());
            if html.is_empty() {
                println!("[DEBUG] WARNING: index.html content is empty!");
            }
            Html(html).into_response()
        }
        Err(e) => {
            eprintln!("[DEBUG] ERROR: Failed to read index.html from path '{:?}': {:?}", path, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Critical Server Error: Could not load web interface. Details: {}", e),
            )
                .into_response()
        }
    }
}

async fn websocket_handler(ws: WebSocketUpgrade, State(state): State<ApiState>) -> impl IntoResponse {
    ws.on_upgrade(|socket| websocket(socket, state))
}

async fn websocket(socket: WebSocket, state: ApiState) {
    let (mut sender, _) = socket.split();
    let mut interval = IntervalStream::new(interval(Duration::from_secs(1)));
    while let Some(_) = interval.next().await {
        let metadata_guard = state.metadata.lock().await;
        let workers_guard = state.workers.lock().await;
        let payload = StatusPayload {
            metadata: metadata_guard.clone(),
            workers: workers_guard.clone(),
        };
        let payload_str = serde_json::to_string(&payload).unwrap();
        if sender.send(AxumMessage::Text(payload_str)).await.is_err() {
            break;
        }
    }
}

async fn get_task(State(state): State<ApiState>, headers: HeaderMap, ConnectInfo(addr): ConnectInfo<SocketAddr>) -> Result<Json<Chunk>, StatusCode> {
    if let Some(required_pin) = &state.pin_code {
        if let Some(provided_pin) = headers.get("X-ThungaSync-PIN").and_then(|v| v.to_str().ok()) {
            if required_pin != provided_pin {
                return Err(StatusCode::UNAUTHORIZED);
            }
        } else {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }
    let mut meta = state.metadata.lock().await;
    let mut workers = state.workers.lock().await;
    let worker_id = addr.ip().to_string();
    workers.entry(worker_id.clone()).or_insert_with(|| WorkerState {
        id: worker_id.clone(),
        current_chunk: None,
        progress: 0,
    });
    let chunk_to_process = {
        let mut found_chunk: Option<Chunk> = None;
        if let Some(chunk_to_do) = meta.chunks.iter_mut().find(|c| c.state == ChunkState::Pending) {
            chunk_to_do.state = ChunkState::InProgress;
            chunk_to_do.worker_id = Some(worker_id.clone());
            if let Some(worker_state) = workers.get_mut(&worker_id) {
                worker_state.current_chunk = Some(chunk_to_do.id);
                worker_state.progress = 0;
            }
            found_chunk = Some(chunk_to_do.clone());
        }
        found_chunk
    };
    if let Some(chunk) = chunk_to_process {
        if DownloadManager::save_metadata_static(&state.meta_path, &*meta).is_err() {
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
        Ok(Json(chunk))
    } else {
        Err(StatusCode::NO_CONTENT)
    }
}

async fn submit_chunk(Path(chunk_id): Path<u64>, State(state): State<ApiState>, ConnectInfo(addr): ConnectInfo<SocketAddr>, body: Bytes) -> StatusCode {
    let mut meta = state.metadata.lock().await;
    if let Some(chunk) = meta.chunks.iter_mut().find(|c| c.id == chunk_id) {
        if chunk.worker_id.as_deref() != Some(&addr.ip().to_string()) {
            return StatusCode::FORBIDDEN;
        }
        if chunk.state == ChunkState::InProgress {
            let mut fm = state.file_manager.lock().await;
            if fm.write_chunk(chunk.start, &body).await.is_ok() {
                chunk.state = ChunkState::Completed;
                chunk.worker_id = None;
                drop(fm);
                let mut workers = state.workers.lock().await;
                let worker_id = addr.ip().to_string();
                if let Some(worker) = workers.get_mut(&worker_id) {
                    worker.current_chunk = None;
                    worker.progress = 0;
                }
                drop(workers);
                if let Err(e) = DownloadManager::save_metadata_static(&state.meta_path, &*meta) {
                    eprintln!("[P2P Master API] CRITICAL: Failed to save metadata for chunk #{}: {}", chunk_id, e);
                    return StatusCode::INTERNAL_SERVER_ERROR;
                }
                return StatusCode::OK;
            } else {
                return StatusCode::INTERNAL_SERVER_ERROR;
            }
        } else {
            return StatusCode::CONFLICT;
        }
    }
    StatusCode::BAD_REQUEST
}

async fn update_progress(Path(chunk_id): Path<u64>, State(state): State<ApiState>, ConnectInfo(addr): ConnectInfo<SocketAddr>, Json(payload): Json<ProgressUpdate>) -> StatusCode {
    let mut workers = state.workers.lock().await;
    let worker_id = addr.ip().to_string();
    if let Some(worker) = workers.get_mut(&worker_id) {
        if worker.current_chunk == Some(chunk_id) {
            worker.progress = payload.bytes_downloaded;
            return StatusCode::OK;
        }
    }
    StatusCode::BAD_REQUEST
}

async fn heartbeat() -> StatusCode {
    StatusCode::OK
}

async fn get_status(State(state): State<ApiState>) -> Json<StatusPayload> {
    let metadata_guard = state.metadata.lock().await;
    let workers_guard = state.workers.lock().await;
    let payload = StatusPayload {
        metadata: metadata_guard.clone(),
        workers: workers_guard.clone(),
    };
    Json(payload)
}

