# kodegen-server-http

[![License](https://img.shields.io/badge/license-Apache--2.0%20OR%20MIT-blue.svg)](LICENSE.md)

HTTP/HTTPS server infrastructure for building MCP (Model Context Protocol) tools servers.

## Overview

`kodegen-server-http` is a Rust library that provides the foundation for creating category-specific MCP servers that expose tools and prompts via HTTP. It handles all the boilerplate including:

- HTTP/HTTPS server setup with optional TLS
- MCP protocol implementation via [rmcp](https://github.com/rrrodzilla/rmcp) Streamable HTTP transport
- Graceful shutdown coordination
- Configuration management
- Usage tracking
- Tool and prompt routing
- Manager lifecycle coordination

This library is designed to be used by category servers (filesystem, browser, database, etc.) - it is **not** a standalone application.

## Features

- 🚀 **Easy Integration** - Single `run_http_server()` call handles all setup
- 🔒 **TLS Support** - Optional HTTPS with certificate-based encryption
- 🎯 **Graceful Shutdown** - Coordinated shutdown of HTTP server and managed resources
- 📊 **Built-in Tracking** - Automatic usage tracking and tool history
- 🔄 **Stateful Sessions** - Support for stateful HTTP sessions with SSE keep-alive
- 🌐 **CORS Enabled** - Permissive CORS for cross-origin requests

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
kodegen-server-http = "0.1"
kodegen-tools-config = "0.1"
kodegen-utils = "0.1"
kodegen-mcp-tool = "0.1"
rmcp = { version = "0.8", features = ["server", "transport-streamable-http-server"] }
tokio = { version = "1", features = ["full"] }
anyhow = "1"
```

## Quick Start

Create a category server in `main.rs`:

```rust
use kodegen_server_http::{run_http_server, RouterSet, Managers, register_tool};
use rmcp::handler::server::router::{prompt::PromptRouter, tool::ToolRouter};
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    run_http_server("my-category", |config, tracker| {
        let mut tool_router = ToolRouter::new();
        let mut prompt_router = PromptRouter::new();
        let mut managers = Managers::new();

        // Register your tools
        (tool_router, prompt_router) = register_tool(
            tool_router,
            prompt_router,
            MyTool::new(config.clone()),
        );

        // Register managers that need shutdown
        // let my_manager = Arc::new(MyManager::new());
        // managers.register(my_manager.clone());

        Ok(RouterSet::new(tool_router, prompt_router, managers))
    }).await
}
```

## CLI Usage

```bash
# Basic HTTP server
cargo run -- --http 127.0.0.1:8080

# HTTPS with TLS
cargo run -- --http 127.0.0.1:8443 \
  --tls-cert /path/to/cert.pem \
  --tls-key /path/to/key.pem

# Custom shutdown timeout
cargo run -- --http 127.0.0.1:8080 --shutdown-timeout-secs 60
```

### CLI Options

| Option | Required | Description | Default |
|--------|----------|-------------|---------|
| `--http <ADDRESS>` | Yes | HTTP server bind address (e.g., `127.0.0.1:8080`) | - |
| `--tls-cert <PATH>` | No | Path to TLS certificate file (enables HTTPS) | - |
| `--tls-key <PATH>` | No | Path to TLS private key file | - |
| `--shutdown-timeout-secs <SECONDS>` | No | Graceful shutdown timeout | 30 |

**Note**: Both `--tls-cert` and `--tls-key` must be provided together to enable HTTPS.

## Architecture

### Inversion of Control

The library uses an inversion of control pattern. You provide a registration callback to `run_http_server()`, and the library handles:

1. Environment initialization (logging, rustls crypto provider)
2. CLI argument parsing
3. Configuration and usage tracker setup
4. Calling your registration callback to build routers
5. HTTP/HTTPS server creation and startup
6. Signal handling (SIGINT, SIGTERM, SIGHUP)
7. Graceful shutdown coordination

### Components

#### `RouterSet<S>`

Container holding your tool router, prompt router, and managers.

```rust
pub struct RouterSet<S> {
    pub tool_router: ToolRouter<S>,
    pub prompt_router: PromptRouter<S>,
    pub managers: Managers,
}
```

#### `HttpServer`

The MCP server implementation that serves tools via Streamable HTTP. Implements the `rmcp::ServerHandler` trait.

#### `Managers`

Container for components requiring graceful shutdown (browsers, tunnels, background tasks, etc.).

```rust
let my_manager = Arc::new(MyManager::new());
managers.register(my_manager.clone());
```

Your manager must implement the `ShutdownHook` trait:

```rust
impl ShutdownHook for MyManager {
    fn shutdown(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // Cleanup logic here
            Ok(())
        })
    }
}
```

#### Tool Registration

Two helper functions for registering tools:

```rust
// Takes ownership, wraps in Arc
let (tool_router, prompt_router) = register_tool(
    tool_router,
    prompt_router,
    MyTool::new(config.clone()),
);

// For pre-Arc'd tools (when you need a reference)
let my_tool = Arc::new(MyTool::new(config.clone()));
let (tool_router, prompt_router) = register_tool_arc(
    tool_router,
    prompt_router,
    my_tool.clone(),
);
```

## Graceful Shutdown

The library implements a coordinated graceful shutdown:

1. **Signal Received** - SIGINT/SIGTERM/SIGHUP (Unix) or Ctrl+C (Windows)
2. **Shutdown Initiated** - CancellationToken triggers
3. **Parallel Shutdown**:
   - HTTP server begins graceful shutdown (20s timeout)
   - Manager shutdown starts after 2s delay (allows in-flight requests to complete)
4. **Completion** - Waits up to `--shutdown-timeout-secs` for all to complete

## Development

```bash
# Build
cargo build

# Run tests
cargo test

# Run Clippy
cargo clippy

# Format code
cargo fmt

# Build release
cargo build --release
```

## Requirements

- **Rust**: Nightly toolchain (see `rust-toolchain.toml`)
- **Components**: rustfmt, clippy
- **Targets**: x86_64-apple-darwin, wasm32-unknown-unknown

## License

Dual-licensed under Apache 2.0 or MIT terms. See [LICENSE.md](LICENSE.md) for details.

## Related Projects

- [rmcp](https://github.com/rrrodzilla/rmcp) - Rust MCP SDK
- [Model Context Protocol](https://modelcontextprotocol.io/) - MCP specification

## Author

**KODEGEN.ᴀɪ**
- Homepage: [kodegen.ai](https://kodegen.ai)
- Repository: [github.com/cyrup-ai/kodegen-server-http](https://github.com/cyrup-ai/kodegen-server-http)
