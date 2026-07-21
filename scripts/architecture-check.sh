#!/bin/sh
# Keep v1's security and integration boundaries mechanically visible in CI.
set -eu

root=${1:?usage: architecture-check.sh <plugin-root>}
worker_source="$root/worker/src"
worker_manifest="$root/worker/Cargo.toml"
lua_source="$root/lua/obsidian-base"

if ! rg -n '^#!\[forbid\(unsafe_code\)\]$' "$worker_source/lib.rs" >/dev/null; then
  echo "the Bases worker must forbid unsafe Rust" >&2
  exit 1
fi
if rg -n '^[[:space:]]*(rhai|evalexpr|rune|mlua|boa_engine|deno_core)[[:space:]]*=' "$worker_manifest" \
    || rg -n 'std::process|tokio::process|Command::new|TcpStream|UdpSocket|tokio::net' "$worker_source" --glob '*.rs'; then
  echo "dynamic evaluation, networking, and runtime subprocesses are forbidden in the Bases worker" >&2
  exit 1
fi
if rg -n 'OBSIDIAN_BASE_CAPTURE|obsidian[^[:space:]]*cli' "$worker_source" "$lua_source"/*.lua --glob '*.rs' --glob '*.lua'; then
  echo "runtime Obsidian CLI integration is forbidden" >&2
  exit 1
fi
if rg -n 'vim\.diagnostic|publishDiagnostics|vim\.lsp\.handlers|textDocument/(completion|hover|documentSymbol)' "$lua_source"/*.lua; then
  echo "diagnostics and custom LSP features are out of scope for v1" >&2
  exit 1
fi
if rg -n 'require\(["'"'"']obsidian\.' "$lua_source"; then
  echo "Bases must not import obsidian.nvim private modules" >&2
  exit 1
fi
