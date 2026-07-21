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

local query_envelope = {
  id = 2,
  response = {
    type = "success",
    result = {
      method = "query",
      data = {
        result_id = "r1-1", source_id = "fixture", view = { name = "Table", type = "table" },
        available_views = { { name = "Table", type = "table" } }, columns = {}, preview_rows = {},
        matched_count = 0, view_count = 0, preview_count = 0, truncated = false,
        warnings = {}, timings = {}, index_generation = 1,
      },
    },
  },
}
assert_true(contract.validate_envelope(query_envelope), "query envelope rejected")
local query, query_error = contract.decode_response(query_envelope, "query")
assert_true(query and not query_error and query.available_views[1].name == "Table", "query decoder lost views")
query_envelope.response.result.data.available_views[1].extra = true
local decoded, decode_error = contract.decode_response(query_envelope, "query")
assert_true(decoded == nil and decode_error.code == "protocol", "unknown query field should be rejected")

local opts = config.setup({ worker_path = "/tmp/obsidian-base-worker", max_preview_rows = 10, limits = { query_ms = 500 } })
assert_true(opts.worker_path == "/tmp/obsidian-base-worker", "worker path override lost")
assert_true(opts.max_preview_rows == 10, "config override lost")
assert_true(opts.limits.query_ms == 500, "nested config override lost")
assert_true(opts.limits.source_bytes == nil, "worker default limits must not be mirrored in Lua")
assert_true(vim.json.encode(config.setup({}).limits) == "{}", "empty worker limit patch must encode as an object")
assert_true(not pcall(config.setup, { debounce_ms = 150 }), "removed debounce option should fail")
assert_true(not pcall(config.setup, { node = "node" }), "legacy Node option should fail")
assert_true(not pcall(config.setup, { unknown = true }), "unknown options should fail")

print("obsidian-base configuration smoke passed")
