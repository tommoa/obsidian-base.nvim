-- Exercises workspace ownership through real buffer, embed, and worker requests.
vim.opt.runtimepath:append(vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":h:h"))
vim.o.hidden = true

local bases = require("obsidian-base")
local coordinator = require("obsidian-base.coordinator")
bases.setup()

local root = vim.fn.tempname()
vim.fn.mkdir(root .. "/.obsidian", "p")
vim.fn.writefile({ "# First" }, root .. "/First.md")
vim.fn.writefile({ "# Second" }, root .. "/Second.md")
vim.fn.writefile({ "views:", "  - type: table", "    name: Table", "    order: [file.name]" }, root .. "/Tasks.base")

local function open(name)
  vim.cmd.edit(vim.fn.fnameescape(root .. "/" .. name))
  local bufnr = vim.api.nvim_get_current_buf()
  vim.bo[bufnr].filetype = "markdown"
  bases.refresh(bufnr)
  return bufnr
end

local function settled(workspace)
  return workspace.status == "ready" and not next(workspace.pending)
end

local function stopped(workspace)
  return workspace.status == "stopping" and workspace.handle == nil
end

local first = open("First.md")
local second = open("Second.md")
local workspace = coordinator.state(first).workspace
assert(coordinator.state(second).workspace == workspace, "buffers did not share a workspace")
assert(vim.wait(5000, function() return settled(workspace) end, 25), "worker did not settle")

vim.api.nvim_buf_delete(first, { force = true })
assert(workspace.status == "ready" and workspace.buffers[second], "first detach stopped a shared worker")

local unsubscribe = coordinator.subscribe_index(root, function() end)
vim.api.nvim_buf_delete(second, { force = true })
assert(vim.wait(5000, function() return settled(workspace) end, 25),
  "last buffer detach interrupted an index subscriber")

local embed_error, embed_result
coordinator.query_embed({
  bufnr = second,
  workspace_root = root,
  host_path = root .. "/Second.md",
  source_path = "Tasks.base",
}, function(error, result)
  embed_error, embed_result = error, result
end)
assert(vim.wait(5000, function() return embed_error or embed_result end, 25), "embed query timed out")
assert(not embed_error and embed_result, "embed query lost its subscribed worker: " .. vim.inspect(embed_error))
assert(workspace.status == "ready", "embed subscription did not retain the worker")

unsubscribe()
assert(vim.wait(5000, function() return stopped(workspace) end, 25),
  "unsubscribing did not stop the idle worker")

local third = open("First.md")
local replacement = coordinator.state(third).workspace
assert(replacement ~= workspace, "new attachment reused a stopped worker")
assert(vim.wait(5000, function() return settled(replacement) end, 25), "replacement worker did not settle")

local inspect_error, inspected
coordinator.inspect_worker(third, function(error, result)
  inspect_error, inspected = error, result
end)
vim.api.nvim_buf_delete(third, { force = true })
assert(next(replacement.pending), "test did not leave a request in flight")
assert(replacement.status == "ready", "last detach stopped an in-flight request")
assert(vim.wait(5000, function() return inspect_error or inspected end, 25), "in-flight request timed out")
assert(not inspect_error and inspected, "in-flight request failed: " .. vim.inspect(inspect_error))
assert(vim.wait(5000, function() return stopped(replacement) end, 25),
  "completed request did not release the idle worker")

local standalone_error, standalone_result
coordinator.query_embed({
  bufnr = third,
  workspace_root = root,
  host_path = root .. "/First.md",
  source_path = "Tasks.base",
}, function(error, result)
  standalone_error, standalone_result = error, result
end)
assert(vim.wait(5000, function() return standalone_error or standalone_result end, 25),
  "standalone embed query timed out")
assert(not standalone_error and standalone_result, "standalone embed query failed: " .. vim.inspect(standalone_error))

vim.fn.delete(root, "rf")
print("obsidian-base lifecycle smoke passed")
