use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::deepseek::ChatMessage;

const PROTOCOL_VERSION: u64 = 1;

#[derive(Debug, Clone)]
pub struct BrowserSidecarConfig {
    pub script_path: PathBuf,
    pub profile_path: Option<PathBuf>,
    pub request_timeout: Duration,
}

impl BrowserSidecarConfig {
    pub fn discover() -> anyhow::Result<Self> {
        let script_path = if let Ok(path) = std::env::var("CHAMPR_DEEPSEEK_WEB_SIDECAR_PATH") {
            PathBuf::from(path)
        } else {
            discover_script_path()?
        };
        let profile_path = std::env::var("CHAMPR_DEEPSEEK_WEB_PROFILE")
            .ok()
            .map(PathBuf::from);
        Ok(Self {
            script_path,
            profile_path,
            request_timeout: Duration::from_secs(90),
        })
    }
}

fn discover_script_path() -> anyhow::Result<PathBuf> {
    let relative = Path::new("packages/deepseek-web/dist/main.js");
    if relative.exists() {
        return relative.canonicalize().context("failed to resolve DeepSeek Web sidecar path");
    }
    let executable = std::env::current_exe().context("failed to resolve current executable")?;
    let packaged = executable
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("deepseek-web/main.js");
    if packaged.exists() {
        return Ok(packaged);
    }
    bail!(
        "DeepSeek Web sidecar not found; run `pnpm --dir packages/deepseek-web build` or set CHAMPR_DEEPSEEK_WEB_SIDECAR_PATH"
    )
}

struct SidecarProcess {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
}

#[derive(Debug, Deserialize)]
struct WireMessage {
    #[serde(default)]
    id: String,
    #[serde(default)]
    ok: bool,
    result: Option<Value>,
    error: Option<WireError>,
    event: Option<String>,
    version: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct WireError {
    code: String,
    message: String,
}

#[derive(Debug, Deserialize)]
pub struct BrowserStatus {
    pub state: String,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
struct Request<'a> {
    id: &'a str,
    method: &'a str,
    params: Value,
}

pub struct BrowserSidecar {
    config: BrowserSidecarConfig,
    process: Mutex<Option<SidecarProcess>>,
    next_id: AtomicU64,
}

impl BrowserSidecar {
    pub fn new(config: BrowserSidecarConfig) -> Self {
        Self {
            config,
            process: Mutex::new(None),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn discover() -> anyhow::Result<Self> {
        Ok(Self::new(BrowserSidecarConfig::discover()?))
    }

    pub async fn status(&self) -> anyhow::Result<BrowserStatus> {
        let value = self.request("status", json!({})).await?;
        serde_json::from_value(value).context("invalid DeepSeek Web status response")
    }

    pub async fn open_login(&self) -> anyhow::Result<BrowserStatus> {
        let value = self.request("open_login", json!({})).await?;
        serde_json::from_value(value).context("invalid DeepSeek Web login response")
    }

    pub async fn reset(&self) -> anyhow::Result<()> {
        self.request("reset", json!({})).await?;
        Ok(())
    }

    pub async fn chat_messages(&self, messages: Vec<ChatMessage>) -> anyhow::Result<String> {
        let value = self
            .request(
                "send",
                json!({
                    "messages": messages,
                    "timeout_ms": self.config.request_timeout.as_millis() as u64,
                }),
            )
            .await?;
        value
            .get("content")
            .and_then(Value::as_str)
            .filter(|content| !content.trim().is_empty())
            .map(str::to_owned)
            .context("DeepSeek Web returned an empty response")
    }

    async fn request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let mut guard = self.process.lock().await;
        if guard.is_none() {
            *guard = Some(self.spawn().await?);
        }
        let process = guard.as_mut().expect("sidecar process was initialized");
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let request = Request { id: &id, method, params };
        let mut bytes = serde_json::to_vec(&request).context("failed to encode sidecar request")?;
        bytes.push(b'\n');
        process.stdin.write_all(&bytes).await.context("failed to write sidecar request")?;
        process.stdin.flush().await.context("failed to flush sidecar request")?;

        let response = tokio::time::timeout(self.config.request_timeout + Duration::from_secs(5), async {
            while let Some(line) = process.lines.next_line().await.context("failed to read sidecar response")? {
                let message: WireMessage = serde_json::from_str(&line)
                    .with_context(|| format!("invalid sidecar response: {line}"))?;
                if message.id == id {
                    return Ok(message);
                }
            }
            bail!("DeepSeek Web sidecar exited unexpectedly")
        })
        .await
        .context("DeepSeek Web request timed out")??;

        decode_response(response)
    }

    async fn spawn(&self) -> anyhow::Result<SidecarProcess> {
        let mut command = Command::new("node");
        command
            .arg(&self.config.script_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if let Some(profile_path) = &self.config.profile_path {
            command.env("CHAMPR_DEEPSEEK_WEB_PROFILE", profile_path);
        }
        #[cfg(target_os = "windows")]
        command.creation_flags(0x08000000);

        let mut child = command.spawn().context("failed to start DeepSeek Web sidecar")?;
        let stdin = child.stdin.take().context("sidecar stdin is unavailable")?;
        let stdout = child.stdout.take().context("sidecar stdout is unavailable")?;
        let mut lines = BufReader::new(stdout).lines();
        let ready_line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .context("DeepSeek Web sidecar ready handshake timed out")??
            .context("DeepSeek Web sidecar exited before ready handshake")?;
        let ready: WireMessage = serde_json::from_str(&ready_line)
            .context("invalid DeepSeek Web sidecar ready handshake")?;
        if ready.event.as_deref() != Some("ready") || ready.version != Some(PROTOCOL_VERSION) {
            bail!("unsupported DeepSeek Web sidecar protocol");
        }
        Ok(SidecarProcess { child, stdin, lines })
    }
}

fn decode_response(response: WireMessage) -> anyhow::Result<Value> {
    if response.ok {
        return Ok(response.result.unwrap_or(Value::Null));
    }
    if let Some(error) = response.error {
        bail!("DeepSeek Web [{}]: {}", error.code, error.message);
    }
    bail!("DeepSeek Web returned an invalid error response")
}

impl Drop for SidecarProcess {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_success_response() {
        let response = WireMessage {
            id: "1".into(),
            ok: true,
            result: Some(json!({ "content": "建议" })),
            error: None,
            event: None,
            version: None,
        };
        assert_eq!(decode_response(response).unwrap()["content"], "建议");
    }

    #[test]
    fn preserves_structured_error_code() {
        let response = WireMessage {
            id: "1".into(),
            ok: false,
            result: None,
            error: Some(WireError { code: "login_required".into(), message: "请登录".into() }),
            event: None,
            version: None,
        };
        assert!(decode_response(response).unwrap_err().to_string().contains("login_required"));
    }
}