# ruprogress-mcp

A Redmine MCP server in Rust. Exposes Redmine's REST API to MCP clients over
stdio and streamable HTTP, with four authentication modes, a read-only mode,
bounded responses, and a local attachment store.

```sh
cargo install ruprogress-mcp
ruprogress-mcp --print-config   # resolve and check the config, then exit
```

Or run the container image:

```sh
docker run --rm -p 8000:8000 \
  -e REDMINE_URL=https://redmine.example.com \
  -e REDMINE_API_KEY=... \
  ghcr.io/mimi1vx/ruprogress-mcp:latest --transport http
```

See the [repository README](https://github.com/mimi1vx/ruprogress-mcp) for
the full tool list and authentication modes, and
[docs/configuration.md](https://github.com/mimi1vx/ruprogress-mcp/blob/main/docs/configuration.md)
for every environment variable.
