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

local function mkdir_parent(path)
  local parent = vim.fs.dirname(path)
  if uv.fs_stat(parent) then return end
  local ancestor = vim.fs.dirname(parent)
  if ancestor ~= parent then mkdir_parent(parent) end
  local ok, mkdir_error = uv.fs_mkdir(parent, 493)
  if not ok and not uv.fs_stat(parent) then
    fail("could not create " .. parent .. ": " .. tostring(mkdir_error))
  end
end

local function write_file(path, contents)
  mkdir_parent(path)
  local fd, open_error = uv.fs_open(path, "w", 420)
  if not fd then fail("could not open " .. path .. ": " .. tostring(open_error)) end
  local written, write_error = uv.fs_write(fd, contents, 0)
  uv.fs_close(fd)
  if not written then fail("could not write " .. path .. ": " .. tostring(write_error)) end
end

local function stable_json(value, depth)
  depth = depth or 0
  if type(value) ~= "table" then return vim.json.encode(value) end
  if vim.islist(value) then
    if #value == 0 then return "[]" end
    local items = {}
    for index, item in ipairs(value) do
      items[index] = string.rep(" ", (depth + 1) * 2) .. stable_json(item, depth + 1)
    end
    return "[\n" .. table.concat(items, ",\n") .. "\n" .. string.rep(" ", depth * 2) .. "]"
  end
  local keys = vim.tbl_keys(value)
  table.sort(keys)
  if #keys == 0 then return "{}" end
  local fields = {}
  for index, key in ipairs(keys) do
    if type(key) ~= "string" then fail("JSON object contains a non-string key") end
    fields[index] = string.rep(" ", (depth + 1) * 2)
        .. vim.json.encode(key) .. ": " .. stable_json(value[key], depth + 1)
  end
  return "{\n" .. table.concat(fields, ",\n") .. "\n" .. string.rep(" ", depth * 2) .. "}"
end

if vim.env.OBSIDIAN_BASE_CAPTURE ~= "1" then
  fail("refusing capture: set OBSIDIAN_BASE_CAPTURE=1 explicitly")
end

local config_path = arg[1]
if not config_path then
  fail("usage: OBSIDIAN_BASE_CAPTURE=1 nvim --headless -u NONE -i NONE -l scripts/capture-cli-fixtures.lua <capture-config.json>")
end
config_path = vim.fn.fnamemodify(config_path, ":p")

local ok, config = pcall(vim.json.decode, read_file(config_path))
if not ok then fail("invalid capture config JSON: " .. tostring(config)) end
if type(config) ~= "table" or config.schema_version ~= 1 or not vim.islist(config.captures) then
  fail("capture config must contain schema_version 1 and captures[]")
end

for _, capture in ipairs(config.captures) do
  if type(capture) ~= "table" or type(capture.id) ~= "string" or capture.id == ""
      or not vim.islist(capture.command) or #capture.command == 0 then
    fail("each capture requires id and a non-empty command array")
  end
  for _, argument in ipairs(capture.command) do
    if type(argument) ~= "string" or argument == "" then
      fail("capture " .. capture.id .. " has invalid command arguments")
    end
  end
  if type(capture.output) ~= "string" or capture.output == "" then
    fail("capture " .. capture.id .. " requires an output path")
  end
  if capture.cwd ~= nil and (type(capture.cwd) ~= "string" or capture.cwd == "") then
    fail("capture " .. capture.id .. " has an invalid working directory")
  end

  local command = vim.deepcopy(capture.command)
  if command[1] == "$OBSIDIAN_BASE_CLI" then command[1] = vim.env.OBSIDIAN_BASE_CLI end
  if not command[1] or command[1] == "" then
    fail("capture " .. capture.id .. " requires OBSIDIAN_BASE_CLI when using the portable CLI placeholder")
  end
  local cwd = capture.cwd and vim.fs.joinpath(vim.fs.dirname(config_path), capture.cwd) or nil
  local result = vim.system(command, { cwd = cwd, text = true }):wait()
  if result.code ~= 0 then
    fail(string.format("capture %s failed (%d): %s", capture.id, result.code, result.stderr ~= "" and result.stderr or result.stdout))
  end
  local decoded, parsed = pcall(vim.json.decode, result.stdout)
  if not decoded then fail("capture " .. capture.id .. " did not return JSON: " .. tostring(parsed)) end

  local output = vim.fs.joinpath(vim.fs.dirname(config_path), capture.output)
  write_file(output, stable_json(parsed) .. "\n")
  print("captured " .. capture.id .. " -> " .. output)
end
