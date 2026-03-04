mod server;

use clap::Parser;
use rmcp::service::ServiceExt;
use tracing_subscriber::{EnvFilter, fmt};

#[derive(Parser)]
#[command(about = "MCP server for Xilinx XSDB/XSCT hardware debugging")]
struct Args {
    /// Path to the XSDB/XSCT executable
    #[arg(long, env = "XSDB_PATH")]
    xsdb_path: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // All logging to stderr (stdout is the MCP JSON-RPC transport)
    fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    tracing::info!("Starting xsdb-mcp server");

    let server = server::XsdbServer::new(args.xsdb_path);
    let transport = rmcp::transport::io::stdio();
    let running = server.serve(transport).await?;
    running.waiting().await?;

    Ok(())
}
