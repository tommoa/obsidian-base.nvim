-- Defines strict JSON-lines decoding for the native worker boundary.
local M = {}

---@class ObsidianBasesProtocolError
---@field code string
---@field message string
---@field available_views? ObsidianBasesTableView[]

---@class ObsidianBasesResultCell
---@field type string
---@field text string
---@field target? string

---@class ObsidianBasesResultRow
---@field path string
---@field display_name string
---@field cells ObsidianBasesResultCell[]

---@class ObsidianBasesTableView
---@field name string
---@field type "table"

---@class ObsidianBasesTableResult
---@field result_id string
---@field source_id string
---@field view ObsidianBasesTableView
---@field available_views ObsidianBasesTableView[]
---@field columns { key: string, label: string }[]
---@field preview_rows ObsidianBasesResultRow[]
---@field matched_count integer
---@field view_count integer
---@field preview_count integer
---@field truncated boolean
---@field warnings string[]
---@field timings table<string, integer>
---@field index_generation integer

local function protocol_error(message)
  return nil, { code = "protocol", message = message }
end

local function only_keys(value, allowed)
  if type(value) ~= "table" then return false end
  for key in pairs(value) do
    if not allowed[key] then return false end
  end
  return true
end

local function integer(value)
  return type(value) == "number" and value >= 0 and value == math.floor(value)
end

local function array(value, validate)
  if type(value) ~= "table" then return false end
  local length = 0
  for key, item in pairs(value) do
    if type(key) ~= "number" or key < 1 or key ~= math.floor(key) then return false end
    length = math.max(length, key)
    if not validate(item) then return false end
  end
  for index = 1, length do
    if value[index] == nil then return false end
  end
  return true
end

local function string_array(value)
  return array(value, function(item) return type(item) == "string" end)
end

local function table_view(value)
  return only_keys(value, { name = true, type = true })
      and type(value.name) == "string" and value.name ~= "" and value.type == "table"
end

local function cell(value)
  return only_keys(value, { type = true, text = true, target = true })
      and type(value.type) == "string" and type(value.text) == "string"
      and (value.target == nil or type(value.target) == "string")
end

local function row(value)
  return only_keys(value, { path = true, display_name = true, cells = true })
      and type(value.path) == "string" and type(value.display_name) == "string"
      and array(value.cells, cell)
end

local function column(value)
  return only_keys(value, { key = true, label = true })
      and type(value.key) == "string" and type(value.label) == "string"
end

local function timings(value)
  if type(value) ~= "table" then return false end
  for key, duration in pairs(value) do
    if type(key) ~= "string" or not integer(duration) then return false end
  end
  return true
end

local function initialize(value)
  return only_keys(value, { generation = true, files = true })
      and integer(value.generation) and integer(value.files)
end

local function query(value)
  return only_keys(value, {
    result_id = true, source_id = true, view = true, available_views = true,
    columns = true, preview_rows = true, matched_count = true, view_count = true,
    preview_count = true, truncated = true, warnings = true, timings = true,
    index_generation = true,
  })
      and type(value.result_id) == "string" and type(value.source_id) == "string"
      and table_view(value.view) and array(value.available_views, table_view)
      and array(value.columns, column) and array(value.preview_rows, row)
      and integer(value.matched_count) and integer(value.view_count)
      and integer(value.preview_count) and type(value.truncated) == "boolean"
      and string_array(value.warnings) and timings(value.timings)
      and integer(value.index_generation)
end

local function fetch_rows(value)
  return only_keys(value, { result_id = true, rows = true })
      and type(value.result_id) == "string" and array(value.rows, row)
end

local function generation(value)
  return only_keys(value, { generation = true }) and integer(value.generation)
end

local function inspect(value)
  return only_keys(value, {
    generation = true, files = true, overlays = true, skipped_non_utf8 = true,
    skipped_non_utf8_examples = true, watcher_errors = true,
  })
      and integer(value.generation) and integer(value.files)
      and string_array(value.overlays) and integer(value.skipped_non_utf8)
      and string_array(value.skipped_non_utf8_examples) and string_array(value.watcher_errors)
end

local decoders = {
  initialize = initialize,
  query = query,
  fetch_rows = fetch_rows,
  overlay_upsert = generation,
  overlay_commit = generation,
  overlay_remove = generation,
  inspect = inspect,
  shutdown = function(value) return only_keys(value, {}) end,
}

---Validate the outer shape of one worker output envelope.
---@param envelope any
---@return boolean?, ObsidianBasesProtocolError?
function M.validate_envelope(envelope)
  if type(envelope) ~= "table" then return protocol_error("worker envelope must be an object") end
  if envelope.event ~= nil then
    if not only_keys(envelope, { event = true }) or type(envelope.event) ~= "table" then
      return protocol_error("invalid worker event envelope")
    end
    local event = envelope.event
    if not only_keys(event, { type = true, generation = true, paths = true, origin = true })
        or event.type ~= "index_changed" or not integer(event.generation)
        or not string_array(event.paths) or (event.origin ~= "overlay" and event.origin ~= "watch")
    then
      return protocol_error("invalid index_changed event")
    end
    return true
  end
  if not only_keys(envelope, { id = true, response = true })
      or not integer(envelope.id) or type(envelope.response) ~= "table"
  then
    return protocol_error("invalid worker response envelope")
  end
  local response = envelope.response
  if response.type == "error" then
    if not only_keys(response, { type = true, error = true })
        or not only_keys(response.error, { code = true, message = true, available_views = true })
        or type(response.error.code) ~= "string" or type(response.error.message) ~= "string"
        or (response.error.available_views ~= nil
          and not array(response.error.available_views, table_view))
    then
      return protocol_error("invalid worker error response")
    end
    return true
  end
  if response.type == "success" then
    if not only_keys(response, { type = true, result = true })
        or not only_keys(response.result, { method = true, data = true })
        or type(response.result.method) ~= "string" or type(response.result.data) ~= "table"
    then
      return protocol_error("invalid worker success response")
    end
    return true
  end
  return protocol_error("unknown worker response type")
end

---Decode a response for the operation that created its pending callback.
---@param envelope table
---@param expected_method string
---@return table?, ObsidianBasesProtocolError?
function M.decode_response(envelope, expected_method)
  local response = envelope.response
  if response.type == "error" then return nil, response.error end
  if response.result.method ~= expected_method then
    return protocol_error("worker response method does not match request")
  end
  local decoder = decoders[expected_method]
  if not decoder or not decoder(response.result.data) then
    return protocol_error("invalid " .. expected_method .. " response data")
  end
  return response.result.data
end

return M
