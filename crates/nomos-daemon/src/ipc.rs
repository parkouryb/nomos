use crate::actor::ArbiterHandle;
use crate::server::{ArbiterRequest, ArbiterResponse};
use futures::{SinkExt, StreamExt};
use std::path::{Path, PathBuf};
use tokio::net::UnixListener;
use tokio_util::codec::{Framed, LinesCodec};
use tracing::{error, info, warn};

pub struct IpcServer {
    socket_path: PathBuf,
    handle: ArbiterHandle,
}

impl IpcServer {
    pub fn new(socket_path: PathBuf, handle: ArbiterHandle) -> Self {
        Self {
            socket_path,
            handle,
        }
    }

    pub async fn run(self) -> Result<(), anyhow::Error> {
        // Remove stale socket if exists
        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }

        if let Some(parent) = self.socket_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let listener = UnixListener::bind(&self.socket_path)?;
        info!("Nomos IPC Unix socket listening at {:?}", self.socket_path);

        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let handle = self.handle.clone();
                    tokio::spawn(async move {
                        let mut framed = Framed::new(stream, LinesCodec::new());

                        while let Some(line_res) = framed.next().await {
                            match line_res {
                                Ok(line) => {
                                    match serde_json::from_str::<ArbiterRequest>(&line) {
                                        Ok(req) => {
                                            match handle.send(req).await {
                                                Ok(resp) => {
                                                    let resp_str = serde_json::to_string(&resp).unwrap();
                                                    if let Err(e) = framed.send(resp_str).await {
                                                        warn!("Failed to send response to client: {}", e);
                                                        break;
                                                    }
                                                }
                                                Err(e) => {
                                                    let resp_str = serde_json::to_string(&ArbiterResponse::Error(e.to_string())).unwrap();
                                                    let _ = framed.send(resp_str).await;
                                                    break;
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            let resp = ArbiterResponse::Error(format!("Invalid JSON request: {}", e));
                                            let _ = framed.send(serde_json::to_string(&resp).unwrap()).await;
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!("IPC socket frame error: {}", e);
                                    break;
                                }
                            }
                        }
                    });
                }
                Err(e) => {
                    error!("Error accepting Unix socket connection: {}", e);
                }
            }
        }
    }
}

pub fn default_socket_path() -> PathBuf {
    if let Ok(env_sock) = std::env::var("NOMOS_SOCKET") {
        return PathBuf::from(env_sock);
    }
    if Path::new("/run").exists() && std::fs::metadata("/run").map(|m| !m.permissions().readonly()).unwrap_or(false) {
        PathBuf::from("/run/nomos/arbiter.sock")
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".nomos/arbiter.sock")
    }
}
