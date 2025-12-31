use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::process::Command;
use tokio::sync::Notify;

use crate::types::cloudflare::CloudflareConfig;

#[derive(Clone, serde::Serialize)]
struct CloudflareStatusUpdate {
    id: String,
    status: String,
    message: Option<String>,
    url: Option<String>,
}

struct RunningTunnel {
    notify_stop: Arc<Notify>,
    #[allow(dead_code)]
    handle: tauri::async_runtime::JoinHandle<()>,
}

pub struct CloudflareManager {
    tunnels: Arc<Mutex<HashMap<String, RunningTunnel>>>,
}

impl CloudflareManager {
    pub fn new() -> Self {
        Self {
            tunnels: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn connect(&self, app: AppHandle, config: CloudflareConfig) {
        let tunnels = self.tunnels.clone();
        let config_id = config.id.clone();
        
        // Remove existing tunnel if any
        self.disconnect(&config.id);

        let notify_stop = Arc::new(Notify::new());
        let notify_clone = notify_stop.clone();
        let config_clone = config.clone();
        
        let emit_status = move |status: &str, msg: Option<String>, url: Option<String>| {
            let _ = app.emit("cloudflare-status-changed", CloudflareStatusUpdate {
                id: config_clone.id.clone(),
                status: status.to_string(),
                message: msg,
                url,
            });
        };

        let emit_status_clone = emit_status.clone();

        let handle = tauri::async_runtime::spawn(async move {
            emit_status_clone("connecting", Some("Starting tunnel...".into()), None);
            
            loop {
                // For named tunnels with tokens from Cloudflare Dashboard:
                // The ingress rules (including URL routing) are configured in the dashboard
                // So we only need: cloudflared tunnel run --token <token>
                // 
                // For quick tunnels (no token, just expose a port):
                // cloudflared tunnel --url http://localhost:<port>
                let mut cmd = Command::new("cloudflared");
                
                if config.tunnel_token.is_empty() {
                    // Quick tunnel mode - expose local port directly
                    cmd.arg("tunnel");
                    cmd.arg("--url");
                    cmd.arg(format!("http://localhost:{}", config.local_port));
                } else {
                    // Named tunnel mode - use token from dashboard
                    // Ingress rules are configured in Cloudflare Zero Trust dashboard
                    cmd.arg("tunnel");
                    cmd.arg("run");
                    cmd.arg("--token");
                    cmd.arg(&config.tunnel_token);
                }

                emit_status_clone("connecting", Some(format!("Connecting to port {}...", config.local_port)), None);

                cmd.stdout(std::process::Stdio::piped())
                   .stderr(std::process::Stdio::piped())
                   .stdin(std::process::Stdio::null());
                   
                #[cfg(windows)]
                {
                    const CREATE_NO_WINDOW: u32 = 0x08000000;
                    cmd.creation_flags(CREATE_NO_WINDOW);
                }

                cmd.kill_on_drop(true);

                match cmd.spawn() {
                    Ok(mut child) => {
                        emit_status_clone("connecting", Some("Authenticating...".into()), None);
                        
                        let stderr = child.stderr.take();
                        let stdout = child.stdout.take();
                        let emit_output = emit_status_clone.clone();
                        
                        // Read both stdout and stderr for tunnel URL and status
                        let output_reader = async move {
                            use tokio::io::{AsyncBufReadExt, BufReader};
                            
                            let mut detected_url: Option<String> = None;
                            
                            // Handle stderr (cloudflared logs to stderr)
                            if let Some(stderr) = stderr {
                                let reader = BufReader::new(stderr);
                                let mut lines = reader.lines();
                                while let Ok(Some(line)) = lines.next_line().await {
                                    let line_lower = line.to_lowercase();
                                    
                                    // Detect successful connection - cloudflared logs these on success:
                                    // "INF Connection ... registered" or "INF Registered tunnel connection"
                                    // "INF Starting tunnel tunnelID=..."
                                    if (line_lower.contains("connection") && line_lower.contains("registered"))
                                        || (line_lower.contains("registered") && line_lower.contains("tunnel"))
                                        || (line_lower.contains("starting tunnel") && line_lower.contains("tunnelid"))
                                    {
                                        emit_output("connected", Some("Tunnel established".into()), detected_url.clone());
                                    } else if line_lower.contains("tunnel url:") || line.contains(".trycloudflare.com") || line.contains(".cfargotunnel.com") {
                                        // Extract URL from log (for quick tunnels)
                                        if let Some(url_start) = line.find("https://") {
                                            let url = line[url_start..].split_whitespace().next().unwrap_or("");
                                            detected_url = Some(url.to_string());
                                            emit_output("connected", Some("Tunnel ready".into()), detected_url.clone());
                                        }
                                    } else if line_lower.contains("failed") || (line_lower.contains("error") && !line_lower.contains("loglevel")) {
                                        // Ignore "loglevel" mentions which are just config info
                                        emit_output("error", Some(line.clone()), None);
                                    } else if line_lower.contains("ingress") && line_lower.contains("registered") {
                                        emit_output("connected", Some("Tunnel active".into()), detected_url.clone());
                                    } else if line_lower.contains("initial protocol") || line_lower.contains("connector id") {
                                        // These indicate successful startup
                                        emit_output("connected", Some("Tunnel connected".into()), detected_url.clone());
                                    }
                                }
                            }
                            
                            // Also check stdout
                            if let Some(stdout) = stdout {
                                let reader = BufReader::new(stdout);
                                let mut lines = reader.lines();
                                while let Ok(Some(line)) = lines.next_line().await {
                                    if line.contains("https://") && (line.contains(".trycloudflare.com") || line.contains(".cfargotunnel.com")) {
                                        if let Some(url_start) = line.find("https://") {
                                            let url = line[url_start..].split_whitespace().next().unwrap_or("");
                                            emit_output("connected", Some("Tunnel ready".into()), Some(url.to_string()));
                                        }
                                    }
                                }
                            }
                            
                            None::<String>
                        };

                        tokio::select! {
                            exit_status = child.wait() => {
                                match exit_status {
                                    Ok(status) => {
                                        if status.success() {
                                            emit_status_clone("disconnected", Some("Closed normally".into()), None);
                                        } else {
                                            emit_status_clone("error", Some(format!("Exited code: {:?}", status.code())), None);
                                        }
                                    }
                                    Err(e) => {
                                        emit_status_clone("error", Some(format!("Wait error: {}", e)), None);
                                    }
                                }
                            }
                            _ = output_reader => {
                                let _ = child.wait().await;
                            }
                            _ = notify_clone.notified() => {
                                let _ = child.kill().await;
                                emit_status_clone("disconnected", Some("User disconnected".into()), None);
                                break;
                            }
                        }
                    },
                    Err(e) => {
                        let error_msg = if e.kind() == std::io::ErrorKind::NotFound {
                            "cloudflared not found. Please install it first.".to_string()
                        } else {
                            format!("Failed to start: {}", e)
                        };
                        emit_status_clone("error", Some(error_msg), None);
                    }
                }
                
                // Retry logic
                emit_status_clone("reconnecting", Some("Retrying in 5s...".into()), None);
                
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                    _ = notify_clone.notified() => {
                        emit_status_clone("disconnected", Some("User disconnected".into()), None);
                        break;
                    }
                }
            }
        });

        tunnels.lock().unwrap().insert(config_id, RunningTunnel {
            notify_stop,
            handle,
        });
    }

    pub fn disconnect(&self, id: &str) {
        let mut tunnels = self.tunnels.lock().unwrap();
        if let Some(tunnel) = tunnels.remove(id) {
            tunnel.notify_stop.notify_one();
        }
    }
    
    #[allow(dead_code)]
    pub fn disconnect_all(&self) {
        println!("[Cloudflare Manager] Shutting down all tunnels...");
        let mut tunnels = self.tunnels.lock().unwrap();
        for (id, tunnel) in tunnels.iter() {
            println!("[Cloudflare Manager] Stopping tunnel: {}", id);
            tunnel.notify_stop.notify_one();
        }
        tunnels.clear();
    }

    #[allow(dead_code)]
    pub fn get_status(&self, id: &str) -> String {
       let tunnels = self.tunnels.lock().unwrap();
       if tunnels.contains_key(id) {
           "active".to_string()
       } else {
           "inactive".to_string()
       }
    }
}
