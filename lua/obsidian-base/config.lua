-- Owns validation, defaulting, and retrieval of the plugin's user configuration.
local M = {}

---@class ObsidianBasesLimitsPatch
---@field source_bytes? integer
---@field expression_bytes? integer
---@field query_ms? integer
---@field evaluation_steps? integer
---@field result_rows? integer
---@field result_bytes? integer

---@class ObsidianBasesPresentation
---@field rail string
---@field highlights table<string, string>

---@class ObsidianBasesConfig
---@field worker_path? string
---@field max_preview_rows integer
---@field presentation ObsidianBasesPresentation
---@field limits ObsidianBasesLimitsPatch

---@type ObsidianBasesConfig
local defaults = {
  worker_path = nil,
  max_preview_rows = 50,
  presentation = {
    rail = "│ ",
    highlights = {
      header = "ObsidianBasesHeader",
      border = "ObsidianBasesBorder",
      text = "ObsidianBasesText",
      link = "ObsidianBasesLink",
      warning = "ObsidianBasesWarning",
      loading = "ObsidianBasesLoading",
    },
  },
  limits = vim.empty_dict(),
}

---@type ObsidianBasesConfig
local opts = vim.deepcopy(defaults)

---Reject keys that are not part of a configuration object's supported schema.
---@param value table|nil Candidate configuration object.
---@param allowed table<string, boolean> Permitted keys.
---@param prefix string Key-path prefix for error messages.
local function reject_unknown(value, allowed, prefix)
  for key in pairs(value or {}) do
    if type(key) ~= "string" then error("obsidian-base option keys must be strings", 3) end
    if not allowed[key] then
      error("unknown obsidian-base option: " .. prefix .. key, 3)
    end
  end
end

---Validate a resource limit or count that must be a positive whole number.
---@param value unknown
---@param name string Option name used in the error message.
local function positive_integer(value, name)
  if type(value) ~= "number" or value < 1 or value ~= math.floor(value) then
    error(name .. " must be a positive integer", 3)
  end
end

---Validate and install plugin configuration.
---@param user_opts? table
---@return ObsidianBasesConfig
function M.setup(user_opts)
  user_opts = user_opts or {}
  if type(user_opts) ~= "table" then
    error("obsidian-base setup options must be a table", 2)
  end

  reject_unknown(user_opts, {
    worker_path = true,
    max_preview_rows = true,
    presentation = true,
    limits = true,
  }, "")

  local presentation = user_opts.presentation or {}
  local limits = user_opts.limits or {}
  if type(presentation) ~= "table" then error("presentation must be a table", 2) end
  if type(limits) ~= "table" then error("limits must be a table", 2) end
  reject_unknown(presentation, { rail = true, highlights = true }, "presentation.")
  reject_unknown(limits, {
    source_bytes = true,
    expression_bytes = true,
    query_ms = true,
    evaluation_steps = true,
    result_rows = true,
    result_bytes = true,
  }, "limits.")

  local highlights = presentation.highlights or {}
  if type(highlights) ~= "table" then error("presentation.highlights must be a table", 2) end
  reject_unknown(highlights, {
    header = true,
    border = true,
    text = true,
    link = true,
    warning = true,
    loading = true,
  }, "presentation.highlights.")

  if user_opts.worker_path ~= nil
      and (type(user_opts.worker_path) ~= "string" or user_opts.worker_path == "") then
    error("worker_path must be a non-empty executable path string or nil", 2)
  end
  if user_opts.max_preview_rows ~= nil then positive_integer(user_opts.max_preview_rows, "max_preview_rows") end
  if presentation.rail ~= nil and type(presentation.rail) ~= "string" then
    error("presentation.rail must be a string", 2)
  end
  for key, value in pairs(highlights) do
    if type(value) ~= "string" or value == "" then
      error("presentation.highlights." .. key .. " must be a non-empty string", 2)
    end
  end
  for key, value in pairs(limits) do positive_integer(value, "limits." .. key) end

  opts = vim.deepcopy(defaults)
  opts.worker_path = user_opts.worker_path
  opts.max_preview_rows = user_opts.max_preview_rows or defaults.max_preview_rows
  opts.presentation.rail = presentation.rail or defaults.presentation.rail
  for key, value in pairs(highlights) do opts.presentation.highlights[key] = value end
  opts.limits = next(limits) and vim.deepcopy(limits) or vim.empty_dict()
  return vim.deepcopy(opts)
end

---Return a defensive copy of the current normalised configuration.
---@return ObsidianBasesConfig
function M.get()
  return vim.deepcopy(opts)
end

return M
