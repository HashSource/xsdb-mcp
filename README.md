# xsdb-mcp

MCP server for Xilinx XSDB/XSCT — hardware debugging, FPGA programming, and TCL scripting from AI assistants.

## Install

```sh
cargo install --path .
```

## Configure

Add to your MCP client settings (e.g. `~/.claude/settings.json`):

```json
{
  "mcpServers": {
    "xsdb": {
      "command": "xsdb-mcp",
      "args": ["--xsdb-path", "/opt/Xilinx/2025.2/Vivado_Lab/bin/xsdb"]
    }
  }
}
```

The XSDB path can also be set via the `XSDB_PATH` environment variable or passed per-call in `xsdb_connect`.

## Tools

| Tool | Description |
|-|-|
| `xsdb_connect` | Spawn XSDB and connect to its TCP server |
| `xsdb_eval` | Send a TCL command and return the result |
| `xsdb_status` | Report connection state, PID, and port |
| `xsdb_disconnect` | Close connection and kill the process |

## Usage

```
xsdb_connect          → connects using configured path
xsdb_eval "pid"       → returns XSDB process ID
xsdb_eval "targets"   → list available debug targets
xsdb_eval "fpga -f design.bit" → program FPGA
xsdb_disconnect       → clean shutdown
```

## How it works

The server spawns XSDB with `xsdbserver start` in interactive mode, connects via TCP, and exchanges TCL commands using the XSDB wire protocol (`okay <result>\r\n` / `error <msg>\r\n`). Communication with the MCP client uses JSON-RPC over stdio.

## License

MIT
