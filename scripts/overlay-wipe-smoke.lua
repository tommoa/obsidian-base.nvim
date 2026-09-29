-- Exercise a delayed overlay failure on a live buffer and after BufWipeout.
vim.opt.runtimepath:append(vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":h:h"))

local root = vim.fn.tempname()
local vault = root .. "/vault"
local base = vault .. "/Delayed.base"
local worker = root .. "/worker"
vim.fn.mkdir(vault .. "/.obsidian", "p")
vim.fn.writefile({ "views:", "  - type: table", "    name: Delayed" }, base)
vim.fn.writefile({
  "#!/bin/sh",
  "IFS= read -r request || exit 0",
  [[printf '{"id":1,"response":{"type":"success","result":{"method":"initialize","data":{"generation":1,"files":1}}}}\n']],
  "while IFS= read -r request; do :; done",
}, worker)
assert(vim.fn.setfperm(worker, "rwx------") == 1, "could not make delayed worker executable")

vim.env.OBSIDIAN_BASE_WORKER = nil
local bases = require("obsidian-base")
local coordinator = require("obsidian-base.coordinator")
local presenter = require("obsidian-base.presenter")
bases.setup({ worker_path = worker })
vim.cmd.edit(vim.fn.fnameescape(base))
local bufnr = vim.api.nvim_get_current_buf()
vim.bo[bufnr].filetype = "obsidian_base"
bases.refresh(bufnr)

local function pending_overlay()
  local state = coordinator.state(bufnr)
  if not state or state.workspace.status ~= "ready" then return end
  for id, pending in pairs(state.workspace.pending) do
    if pending.method == "overlay_upsert" then return state.workspace, id, pending.callback end
  end
end

assert(vim.wait(5000, function() return pending_overlay() ~= nil end, 25),
  "overlay request did not remain in flight")
local workspace, id, callback = pending_overlay()
workspace.pending[id] = nil
callback({ code = "worker_unavailable", message = "overlay failed" })
local source = assert(coordinator.inspect(bufnr).sources[1])
assert(source.error and source.error.message == "overlay failed" and not source.loading,
  "overlay failure was not stored for a valid buffer")
local previews = vim.api.nvim_buf_get_extmarks(bufnr, presenter.preview_namespace(), 0, -1, { details = true })
assert(#previews == 1 and previews[1][4].virt_lines[1][1][1]:find("Base error: overlay failed", 1, true),
  "overlay failure was not presented for a valid buffer")

bases.refresh(bufnr)
workspace, id, callback = pending_overlay()
assert(callback, "second overlay request did not remain in flight")
vim.cmd.bwipeout({ bang = true })
assert(not vim.api.nvim_buf_is_valid(bufnr), "test buffer was not wiped")
workspace.pending[id] = nil
local ok, error = pcall(callback, { code = "worker_unavailable", message = "late overlay failure" })
assert(ok, "late overlay failure accessed a wiped buffer: " .. tostring(error))
assert(coordinator.state(bufnr) == nil, "wiped buffer retained coordinator state")

vim.fn.delete(root, "rf")
print("obsidian-base overlay wipe smoke passed")
