# obsidian-base.nvim

`obsidian-base.nvim` renders [Obsidian Bases](https://help.obsidian.md/bases)
in Neovim. It supports standalone `.base` files and `base` fenced blocks in
Markdown, with optional integration into obsidian.nvim commands and code
actions.

## Requirements

- Neovim >= 0.10.0
- Rust 1.85 or newer, or Nix, when building the native worker from source

## Folding

Like obsidian.nvim, this plugin does not override window fold options. To fold
Base blocks and standalone `.base` files, choose its fold expression in a
`FileType` autocmd:

```lua
vim.api.nvim_create_autocmd("FileType", {
  pattern = { "markdown", "obsidian_base" },
  callback = function()
    vim.wo.foldmethod = "expr"
    local level = vim.bo.filetype == "markdown" and 4 or 1
    vim.wo.foldexpr = ("v:lua.require'obsidian-base'.foldexpr(v:lnum, %d)"):format(level)
    vim.wo.foldlevel = vim.bo.filetype == "markdown" and 3 or 99
  end,
})
```

For Markdown buffers, the helper preserves Tree-sitter's fold levels and adds a
nested fold only for Base fences. Keeping `foldlevel` at the normal code-block
level leaves other fenced blocks open while Base fences start closed.
The optional `level` argument is an absolute Base fold level; omit it to use
the natural level one deeper than the fallback fold expression.

Queries run in the native `obsidian-base-worker` process. Startup never
downloads or builds it: install a release binary explicitly, point the plugin
at a packaged binary, or build it from source.

## Installation

With lazy.nvim:

```lua
{
  "obsidian-nvim/obsidian.nvim",
  dependencies = {
    {
      "tommoa/obsidian-base.nvim",
      main = "obsidian-base",
      opts = {},
    },
  },
  opts = {},
}
```

As an obsidian.nvim dependency, setup registers `:Obsidian bases` and its code
actions after obsidian.nvim completes its own setup.

lazy.nvim discovers the repository's `build.lua` and runs its default `auto`
strategy after installation and updates. It downloads the checksummed release
binary when available, then falls back to `cargo build --release --locked`, or
to Nix when Cargo is unavailable or its build fails. Rebuild it with
`:Lazy build obsidian-base.nvim`.

Prebuilt downloads are available only when the checkout is exactly at a release
tag. Branches, commits, and source archives fall back to Cargo or Nix.

Other plugin managers and Lua automation can start an install explicitly:

```lua
require("obsidian-base").install_worker({ strategy = "cargo" }, function(err, result)
  if err then error(err) end
  vim.notify("Obsidian Bases worker is ready: " .. result.path)
end)
```

Prebuilt downloads require Git, `curl`, and `sha256sum` on Linux, `shasum` on
macOS, or `certutil` on Windows. Cargo builds require Rust 1.85 or newer. Nix
builds use the repository's `.#worker` package. Downloaded SHA-256 values are
checked before an existing worker is replaced.
Supported release targets are:

- `x86_64-unknown-linux-musl`
- `aarch64-unknown-linux-musl`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`

Only explicit build hooks or `install_worker()` download or build a worker;
normal startup only resolves existing binaries. Pass `force = true` to replace
a valid managed worker. On Windows, a running worker executable cannot be
replaced in place; reinstall before opening worker-backed buffers, or restart
Neovim first.

To use another native build, set `worker_path`:

```lua
require("obsidian-base").setup({
  worker_path = "/absolute/path/to/obsidian-base-worker",
})
```

`OBSIDIAN_BASE_WORKER` overrides both `worker_path` and automatic resolution.
Without an override, the plugin checks the downloaded worker, a local Cargo
release build, `result/bin`, the installer-managed Nix out-link, and finally
`obsidian-base-worker` on `PATH`.

## Source Build

The worker uses Rust 1.85, edition 2024:

```sh
cargo +1.85 build --manifest-path worker/Cargo.toml --release --locked
```

The resulting executable is
`worker/target/release/obsidian-base-worker` (with `.exe` on Windows) and is
discovered automatically. Packagers can instead build `.#worker` with Nix,
configure `worker_path`, or put `obsidian-base-worker` on `PATH`. The native
worker retains explicit source, expression, evaluation, row, and result-byte
limits, but does not impose a process-wide memory cap.

Date properties accept `YYYY-MM-DD` or RFC 3339 timestamps with an explicit
`Z` or numeric offset. Date-only values mean midnight UTC. Sorting is a
deterministic, case-sensitive natural order with explicit handling for digit
runs and leading zeroes. Non-UTF-8 filesystem entries are skipped and counted
in worker inspection diagnostics.

## Local Checks

```sh
cargo +1.85 fmt --manifest-path worker/Cargo.toml --check
cargo +1.85 clippy --manifest-path worker/Cargo.toml --all-targets --locked -- -D warnings
cargo +1.85 test --manifest-path worker/Cargo.toml --locked
nix build 'path:.#worker'
nix flake check 'path:.'
```

The flake builds and tests the Rust worker, then runs the Lua integration smokes
against the resulting native executable. The embed smoke remains separate
because it depends on private embed-provider modules.

Maintainers should follow the documented [release procedure](docs/releasing.md)
to produce and verify native worker assets.

The CLI capture script is intentionally opt-in and never participates in normal
tests or runtime. With an enabled CLI, run the reviewed live capture using:

Open `fixtures/vault` as a vault in Obsidian once before running this command.
The CLI can only query vaults known to the running Obsidian application.

```sh
OBSIDIAN_BASE_CAPTURE=1 OBSIDIAN_BASE_CLI="$(command -v obsidian)" \
  nvim --headless -u NONE -i NONE -l scripts/capture-cli-fixtures.lua \
  fixtures/cli-capture.json
```

The portable `$OBSIDIAN_BASE_CLI` placeholder avoids committing a
machine-specific CLI path. Review any generated golden change before
committing it. See `scripts/capture-cli-fixtures.lua`.

After review, verify the worker against those direct-Base goldens without
calling Obsidian again:

```sh
OBSIDIAN_BASE_WORKER="$PWD/worker/target/release/obsidian-base-worker" \
  nvim --headless -u NONE -i NONE -l scripts/verify-cli-goldens.lua \
  fixtures/cli-capture.json fixtures/vault
```
