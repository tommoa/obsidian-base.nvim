-- Defines the strict JSON-lines boundary shared with the native worker.
local M = {}

---@class ObsidianBasesProtocolError
---@field code string
---@field message string

---@class ObsidianBasesResultCell
---@field type string
---@field text string
---@field target? string

---@class ObsidianBasesResultRow
---@field path string
---@field display_name string
---@field cells ObsidianBasesResultCell[]

---@class ObsidianBasesTableResult
---@field result_id string
---@field source_id string
---@field view { name: string, type: "table" }
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
  for key in pairs(value) do
    if not allowed[key] then return false end
  end
  return true
end

local function valid_paths(paths)
  if type(paths) ~= "table" then return false end
  for _, path in ipairs(paths) do
    if type(path) ~= "string" then return false end
  end
  return true
end

---Validate one strict worker output envelope.
---@param envelope any
---@return boolean?, ObsidianBasesProtocolError?
function M.validate_envelope(envelope)
  if type(envelope) ~= "table" then return protocol_error("worker envelope must be an object") end
  if envelope.event ~= nil then
    if not only_keys(envelope, { event = true }) or type(envelope.event) ~= "table" then
      return protocol_error("invalid worker event envelope")
    end
    local event = envelope.event
    if not only_keys(event, { type = true, generation = true, paths = true })
        or event.type ~= "index_changed"
        or type(event.generation) ~= "number"
        or event.generation ~= math.floor(event.generation)
        or not valid_paths(event.paths)
    then
      return protocol_error("invalid index_changed event")
    end
    return true
  end
  if not only_keys(envelope, { id = true, response = true })
      or type(envelope.id) ~= "number"
      or envelope.id ~= math.floor(envelope.id)
      or type(envelope.response) ~= "table"
  then
    return protocol_error("invalid worker response envelope")
  end
  local response = envelope.response
  if response.type == "error" then
    if not only_keys(response, { type = true, error = true })
        or type(response.error) ~= "table"
        or type(response.error.code) ~= "string"
        or type(response.error.message) ~= "string"
    then
      return protocol_error("invalid worker error response")
    end
    return true
  end
  if response.type == "success" then
    if not only_keys(response, { type = true, result = true })
        or type(response.result) ~= "table"
        or type(response.result.method) ~= "string"
        or type(response.result.data) ~= "table"
    then
      return protocol_error("invalid worker success response")
    end
    return true
  end
  return protocol_error("unknown worker response type")
end

return M
