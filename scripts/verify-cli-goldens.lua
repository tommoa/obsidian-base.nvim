#!/usr/bin/env -S nvim --headless -u NONE -i NONE -l

local uv = vim.uv

local function fail(message)
  error(message, 0)
end

local function read_file(path)
  local fd, open_error = uv.fs_open(path, "r", 438)
  if not fd then fail("could not open " .. path .. ": " .. tostring(open_error)) end
  local stat, stat_error = uv.fs_fstat(fd)
  if not stat then
    uv.fs_close(fd)
    fail("could not stat " .. path .. ": " .. tostring(stat_error))
  end
  local contents, read_error = uv.fs_read(fd, stat.size, 0)
  uv.fs_close(fd)
  if not contents then fail("could not read " .. path .. ": " .. tostring(read_error)) end
  return contents
end

local function decode_file(path)
  local ok, value = pcall(vim.json.decode, read_file(path))
  if not ok then fail("invalid JSON in " .. path .. ": " .. tostring(value)) end
  return value
end

local function parameter(capture, name)
  for _, argument in ipairs(capture.command or {}) do
    local prefix = name .. "="
    if vim.startswith(argument, prefix) then return argument:sub(#prefix + 1) end
  end
  fail("capture " .. tostring(capture.id) .. " has no " .. name .. "= argument")
end

local config_path, vault_root = arg[1], arg[2]
if not config_path or not vault_root then
  fail("usage: nvim --headless -u NONE -i NONE -l scripts/verify-cli-goldens.lua <capture-config.json> <vault-root> [worker]")
end
local worker = arg[3] or vim.env.OBSIDIAN_BASE_WORKER
if not worker or worker == "" then fail("set OBSIDIAN_BASE_WORKER or pass the native worker path as the third argument") end
worker = vim.fn.fnamemodify(worker, ":p")
if not uv.fs_stat(worker) then fail("native worker not found: " .. worker) end

config_path = vim.fn.fnamemodify(config_path, ":p")
vault_root = vim.fn.fnamemodify(vault_root, ":p")
local config = decode_file(config_path)
if type(config) ~= "table" or config.schema_version ~= 1 or not vim.islist(config.captures) then
  fail("capture config must contain schema_version 1 and captures[]")
end

local requests = {
  vim.json.encode({ id = 1, request = { method = "initialize", params = { vault_root = vault_root } } }),
}
for index, capture in ipairs(config.captures) do
  if type(capture) ~= "table" or type(capture.id) ~= "string" or not vim.islist(capture.command)
      or type(capture.output) ~= "string" then
    fail("each capture requires id, command[], and output")
  end
  local base_path = parameter(capture, "path")
  requests[#requests + 1] = vim.json.encode({
    id = index + 1,
    request = {
      method = "query",
      params = {
        source = { kind = "file", path = base_path, source_id = capture.id },
        host_path = base_path,
        view_name = parameter(capture, "view"),
        preview_rows = 10000,
      },
    },
  })
end
requests[#requests + 1] = vim.json.encode({
  id = #config.captures + 2,
  request = { method = "shutdown", params = {} },
})

local result = vim.system({ worker }, { text = true, stdin = table.concat(requests, "\n") .. "\n" }):wait()
if result.code ~= 0 then
  fail(string.format("native worker failed (%d): %s", result.code, result.stderr ~= "" and result.stderr or result.stdout))
end

local responses = {}
for line in vim.gsplit(result.stdout, "\n", { plain = true, trimempty = true }) do
  local ok, envelope = pcall(vim.json.decode, line)
  if not ok then fail("native worker emitted invalid JSON: " .. line) end
  if envelope.id ~= nil then responses[envelope.id] = envelope end
end
local initialized = responses[1]
if not initialized or initialized.response.type == "error" then
  fail("native worker initialization failed: " .. vim.inspect(initialized and initialized.response.error or "no response"))
end

for index, capture in ipairs(config.captures) do
  local envelope = responses[index + 1]
  if not envelope or envelope.response.type == "error" then
    fail("capture " .. capture.id .. " query failed: " .. vim.inspect(envelope and envelope.response.error or "no response"))
  end
  local expected = decode_file(vim.fs.joinpath(vim.fs.dirname(config_path), capture.output))
  local actual = {}
  for row_index, row in ipairs(envelope.response.result.data.preview_rows or {}) do
    actual[row_index] = { path = row.path, name = row.display_name }
  end
  if not vim.deep_equal(actual, expected) then
    fail(capture.id .. " differs from its CLI golden\nexpected: " .. vim.inspect(expected) .. "\nactual: " .. vim.inspect(actual))
  end
  print("verified " .. capture.id)
end
