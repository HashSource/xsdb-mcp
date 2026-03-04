use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    process::{Child, Command},
    sync::Mutex,
    time::{Duration, sleep, timeout},
};

const DEFAULT_PORT: u16 = 4567;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const READ_TIMEOUT: Duration = Duration::from_secs(30);

struct XsdbState {
    process: Option<Child>,
    reader: Option<BufReader<TcpStream>>,
    port: u16,
}

#[derive(Clone)]
pub struct XsdbServer {
    state: Arc<Mutex<XsdbState>>,
    default_executable: Option<String>,
    tool_router: ToolRouter<Self>,
}

// -- Tool parameter structs --------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConnectParams {
    /// Path to the XSDB/XSCT executable (optional if configured via --xsdb-path or XSDB_PATH)
    pub executable: Option<String>,
    /// TCP port for xsdbserver (default: 4567)
    pub port: Option<u16>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EvalParams {
    /// TCL command to evaluate
    pub command: String,
}

// -- Tool router -------------------------------------------------------------

#[tool_router]
impl XsdbServer {
    pub fn new(default_executable: Option<String>) -> Self {
        Self {
            state: Arc::new(Mutex::new(XsdbState {
                process: None,
                reader: None,
                port: DEFAULT_PORT,
            })),
            default_executable,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Spawn an XSDB/XSCT process and connect to its TCP debug server. Must be called before xsdb_eval."
    )]
    async fn xsdb_connect(
        &self,
        params: Parameters<ConnectParams>,
    ) -> Result<CallToolResult, McpError> {
        let executable = params
            .0
            .executable
            .as_deref()
            .or(self.default_executable.as_deref())
            .ok_or_else(|| {
                McpError::invalid_params(
                    "No executable specified. Pass 'executable' or configure via --xsdb-path / XSDB_PATH.",
                    None,
                )
            })?;
        let port = params.0.port.unwrap_or(DEFAULT_PORT);
        let mut state = self.state.lock().await;

        if state.reader.is_some() {
            return Err(McpError::invalid_params(
                "Already connected. Disconnect first.",
                None,
            ));
        }

        // Spawn XSDB process
        let eval_arg = format!("xsdbserver start -port {port}");
        let child = Command::new(executable)
            .arg("-eval")
            .arg(&eval_arg)
            .arg("-interactive")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                McpError::internal_error(format!("Failed to spawn {executable}: {e}"), None)
            })?;

        let pid = child.id().unwrap_or(0);
        tracing::info!("Spawned XSDB (PID {pid}) on port {port}");
        state.process = Some(child);
        state.port = port;

        // Retry TCP connect until XSDB's server is ready
        let addr = format!("127.0.0.1:{port}");
        let stream = timeout(CONNECT_TIMEOUT, async {
            loop {
                match TcpStream::connect(&addr).await {
                    Ok(s) => return Ok(s),
                    Err(_) => {
                        // Check if process died
                        if let Some(ref mut proc) = state.process {
                            if let Ok(Some(status)) = proc.try_wait() {
                                return Err(McpError::internal_error(
                                    format!("XSDB exited early with status {status}"),
                                    None,
                                ));
                            }
                        }
                        sleep(CONNECT_RETRY_INTERVAL).await;
                    }
                }
            }
        })
        .await
        .map_err(|_| {
            McpError::internal_error(format!("Timed out connecting to XSDB at {addr}"), None)
        })??;

        tracing::info!("Connected to XSDB at {addr}");
        state.reader = Some(BufReader::new(stream));

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Connected to XSDB (PID {pid}) on port {port}"
        ))]))
    }

    #[tool(
        description = "Send a TCL command to the connected XSDB instance and return the result."
    )]
    async fn xsdb_eval(&self, params: Parameters<EvalParams>) -> Result<CallToolResult, McpError> {
        let command = &params.0.command;
        let mut state = self.state.lock().await;

        // Check process is alive
        if let Some(ref mut proc) = state.process {
            if let Ok(Some(status)) = proc.try_wait() {
                state.process = None;
                state.reader = None;
                return Err(McpError::internal_error(
                    format!("XSDB process exited with status {status}"),
                    None,
                ));
            }
        }

        let reader = state.reader.as_mut().ok_or_else(|| {
            McpError::invalid_params("Not connected. Call xsdb_connect first.", None)
        })?;

        // Send command\r\n
        let msg = format!("{command}\r\n");
        reader
            .get_mut()
            .write_all(msg.as_bytes())
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to send command: {e}"), None))?;
        reader
            .get_mut()
            .flush()
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to flush: {e}"), None))?;

        // Read response until \n (protocol sends \r\n)
        let response = timeout(READ_TIMEOUT, async {
            let mut line = String::new();
            reader.read_line(&mut line).await.map_err(|e| {
                McpError::internal_error(format!("Failed to read response: {e}"), None)
            })?;
            Ok::<String, McpError>(line)
        })
        .await
        .map_err(|_| McpError::internal_error("Timed out waiting for XSDB response", None))??;

        let response = response.trim_end_matches("\r\n").trim_end_matches('\n');

        if let Some(result) = response.strip_prefix("okay ") {
            Ok(CallToolResult::success(vec![Content::text(result)]))
        } else if response == "okay" {
            Ok(CallToolResult::success(vec![Content::text("")]))
        } else if let Some(err) = response.strip_prefix("error ") {
            Err(McpError::internal_error(format!("XSDB error: {err}"), None))
        } else {
            Err(McpError::internal_error(
                format!("Unexpected XSDB response: {response}"),
                None,
            ))
        }
    }

    #[tool(description = "Disconnect from the XSDB instance and kill the process.")]
    async fn xsdb_disconnect(&self) -> Result<CallToolResult, McpError> {
        let mut state = self.state.lock().await;

        if state.reader.is_none() && state.process.is_none() {
            return Ok(CallToolResult::success(vec![Content::text(
                "Not connected.",
            )]));
        }

        // Drop TCP stream
        state.reader = None;

        // Kill the process
        if let Some(ref mut proc) = state.process {
            let pid = proc.id();
            proc.kill().await.ok();
            tracing::info!("Killed XSDB process (PID {:?})", pid);
        }
        state.process = None;

        Ok(CallToolResult::success(vec![Content::text(
            "Disconnected from XSDB.",
        )]))
    }

    #[tool(description = "Report the current XSDB connection status, PID, and port.")]
    async fn xsdb_status(&self) -> Result<CallToolResult, McpError> {
        let mut state = self.state.lock().await;

        let connected = state.reader.is_some();
        let port = state.port;

        let (proc_alive, pid) = match state.process {
            Some(ref mut proc) => {
                let pid = proc.id();
                let alive = matches!(proc.try_wait(), Ok(None));
                (alive, pid)
            }
            None => (false, None),
        };

        Ok(CallToolResult::success(vec![Content::text(format!(
            "connected: {connected}\nprocess_alive: {proc_alive}\npid: {}\nport: {port}",
            pid.map_or("none".to_string(), |p| p.to_string())
        ))]))
    }
}

// -- MCP ServerHandler -------------------------------------------------------

#[tool_handler]
impl ServerHandler for XsdbServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        info.server_info.name = "xsdb-mcp".into();
        info.server_info.version = env!("CARGO_PKG_VERSION").into();
        info.instructions = Some(
            "XSDB MCP Server — interact with Xilinx XSDB/XSCT for hardware debugging, \
             FPGA programming, and TCL scripting. Call xsdb_connect first, then xsdb_eval \
             to run commands."
                .into(),
        );
        info
    }
}
