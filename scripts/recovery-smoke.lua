-- Proves that repeated worker crashes are isolated, manually recoverable, and
-- that an invalid Base leaves Neovim responsive enough to recover on refresh.
vim.opt.runtimepath:append(vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":h:h"))

local root = vim.fn.tempname()
local marker = root .. ".crashed"
local wrapper = root .. ".worker-wrapper"
vim.fn.writefile({
  "#!/bin/sh",
  "count=0",
  'if [ -f "$OBSIDIAN_BASE_RECOVERY_CRASH_ONCE" ]; then',
  '  count=$(wc -l < "$OBSIDIAN_BASE_RECOVERY_CRASH_ONCE")',
  "fi",
  'if [ "$count" -lt 3 ]; then',
  '  echo crash >> "$OBSIDIAN_BASE_RECOVERY_CRASH_ONCE"',
  "  exit 97",
  "fi",
  'exec "$OBSIDIAN_BASE_RECOVERY_WORKER" "$@"',
}, wrapper)
assert(vim.fn.setfperm(wrapper, "rwx------") == 1, "could not make worker wrapper executable")
vim.env.OBSIDIAN_BASE_RECOVERY_CRASH_ONCE = marker
local native_worker = vim.env.OBSIDIAN_BASE_WORKER
assert(native_worker and native_worker ~= "", "OBSIDIAN_BASE_WORKER is required")
vim.env.OBSIDIAN_BASE_RECOVERY_WORKER = native_worker
vim.env.OBSIDIAN_BASE_WORKER = nil

local bases = require("obsidian-base")
local coordinator = require("obsidian-base.coordinator")
bases.setup({ worker_path = wrapper })

local script = debug.getinfo(1, "S").source:sub(2)
local vault = vim.fs.joinpath(vim.fn.fnamemodify(script, ":h:h"), "fixtures", "vault")
local base = vim.fs.joinpath(vault, "Bases", "Daily log tasks.base")
vim.cmd.edit(vim.fn.fnameescape(base))
vim.bo.filetype = "obsidian_base"
bases.refresh(0)
assert(vim.wait(10000, function()
  local state = coordinator.inspect(0)
  return state and state.worker_error and state.worker_error:match("crashed repeatedly")
end, 25), "worker did not stop after repeated crashes: " .. vim.inspect(coordinator.inspect(0)))
assert(#vim.fn.readfile(marker) == 3, "expected three failed worker startups")
bases.refresh(0)
assert(vim.wait(10000, function()
  local state = coordinator.inspect(0)
  return state and state.sources[1] and state.sources[1].result
end, 25), "manual refresh did not restart the worker: " .. vim.inspect(coordinator.inspect(0)))

local original = vim.api.nvim_buf_get_lines(0, 0, -1, false)
vim.api.nvim_buf_set_lines(0, 0, -1, false, {
  "filters:",
  "  unexpected: true",
  "views:",
  "  - type: table",
  "    name: Table",
  "    order: [file.name]",
})
bases.refresh(0)
assert(vim.wait(5000, function()
  local snapshot = coordinator.inspect(0)
  local source = snapshot and snapshot.sources[1]
  return source and source.error and source.error.code == "unsupported_filter"
end, 25), "malformed Base did not produce a local structured error: " .. vim.inspect(coordinator.inspect(0)))

vim.api.nvim_buf_set_lines(0, 0, -1, false, original)
bases.refresh(0)
assert(vim.wait(5000, function()
  local source = coordinator.inspect(0).sources[1]
  return source and source.result and not source.error
end, 25), "refresh did not recover after Base query failure")

-- Successful handshakes must not clear crash history. Otherwise a worker that
-- starts and then dies will restart forever without reaching the crash limit.
local post_root = vim.fn.tempname()
local post_vault = post_root .. "/vault"
local post_marker = post_root .. "/crashes"
local post_wrapper = post_root .. "/worker-wrapper"
vim.fn.mkdir(post_vault .. "/.obsidian", "p")
vim.fn.writefile({ "# Host" }, post_vault .. "/Host.md")
vim.fn.writefile({
  "#!/bin/sh",
  'echo crash >> "$OBSIDIAN_BASE_RECOVERY_POST_MARKER"',
  "IFS= read -r request || exit 0",
  [[printf '{"id":1,"response":{"type":"success","result":{"method":"initialize","data":{"generation":1,"files":1}}}}\n']],
  "sleep 0.1",
  "exit 98",
}, post_wrapper)
assert(vim.fn.setfperm(post_wrapper, "rwx------") == 1, "could not make post-handshake wrapper executable")
vim.env.OBSIDIAN_BASE_RECOVERY_POST_MARKER = post_marker
bases.setup({ worker_path = post_wrapper })
vim.cmd.edit(vim.fn.fnameescape(post_vault .. "/Host.md"))
local post_bufnr = vim.api.nvim_get_current_buf()
vim.bo[post_bufnr].filetype = "markdown"
bases.refresh(post_bufnr)
assert(vim.wait(10000, function()
  local state = coordinator.inspect(post_bufnr)
  return state and state.worker_error and state.worker_error:match("crashed repeatedly")
end, 25), "post-handshake worker did not stop after repeated crashes: "
  .. vim.inspect(coordinator.inspect(post_bufnr)))
assert(#vim.fn.readfile(post_marker) == 3, "expected three post-handshake crashes")
vim.fn.delete(post_root, "rf")

vim.env.OBSIDIAN_BASE_WORKER = native_worker
print("obsidian-base recovery smoke passed")
