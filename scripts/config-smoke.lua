-- Verifies Lua-only protocol and configuration behaviour without a worker process.
local script = debug.getinfo(1, "S").source:sub(2)
vim.opt.runtimepath:prepend(vim.fs.dirname(vim.fs.dirname(script)))

---Fail the smoke test with a concise message when a required condition is false.
---@param value unknown
---@param message string
local function assert_true(value, message)
  if not value then error(message, 0) end
end

local contract = require("obsidian-base.contract")
local config = require("obsidian-base.config")

assert_true(contract.validate_envelope({
  id = 1,
  response = { type = "success", result = { method = "initialize", data = { generation = 1, files = 1 } } },
}), "success envelope rejected")
local valid, err = contract.validate_envelope({ id = 1, response = { type = "unknown" } })
assert_true(valid == nil and err.code == "protocol", "unknown response should be rejected")

local opts = config.setup({ worker_path = "/tmp/obsidian-base-worker", max_preview_rows = 10, limits = { query_ms = 500 } })
assert_true(opts.worker_path == "/tmp/obsidian-base-worker", "worker path override lost")
assert_true(opts.max_preview_rows == 10, "config override lost")
assert_true(opts.limits.query_ms == 500, "nested config override lost")
assert_true(not pcall(config.setup, { node = "node" }), "legacy Node option should fail")
assert_true(not pcall(config.setup, { unknown = true }), "unknown options should fail")

print("obsidian-base configuration smoke passed")
