-- Provides picker-driven interactions for selecting Base sources, views, and result rows.
local coordinator = require("obsidian-base.coordinator")
local picker = require("obsidian-base.picker")

local M = {}

---Display a user-facing Bases warning.
---@param message string
local function warn(message)
  vim.notify("Obsidian Bases: " .. message, vim.log.levels.WARN)
end

---@param bufnr integer
---@param callback fun(source_id: string)
---Choose the source at the cursor, or prompt when the buffer has several sources.
local function choose_source(bufnr, callback)
  local source_id = coordinator.source_at_cursor(bufnr)
  if source_id then
    callback(source_id)
    return
  end
  local choices = coordinator.sources(bufnr)
  if #choices == 0 then
    warn("no Base source in this buffer")
    return
  end
  if #choices == 1 then
    callback(choices[1].source_id)
    return
  end
  picker.pick(choices, { title = "Select Base source" }, function(item)
    callback(item.source_id)
  end)
end

---@param bufnr? integer
---@param source_id? string
---Prompt for and apply a named view to a Base source.
function M.select_view(bufnr, source_id)
  bufnr = bufnr or vim.api.nvim_get_current_buf()
  local function select(id)
    if source_id and id ~= source_id then return end
    local views = coordinator.views(bufnr, id)
    if #views == 0 then
      warn("Base view metadata is not ready")
      return
    end
    local choices = vim.tbl_map(function(view)
      return {
        label = view.name .. (view.type and " · " .. view.type or ""),
        name = view.name,
      }
    end, views)
    picker.pick(choices, { title = "Select Base view" }, function(view)
      if not coordinator.select_view(bufnr, id, view.name) then
        warn("Base view is no longer available")
      end
    end)
  end
  if source_id then
    for _, choice in ipairs(coordinator.sources(bufnr)) do
      if choice.source_id == source_id then
        select(choice.source_id)
        return
      end
    end
    warn("Base source is no longer available")
  else
    choose_source(bufnr, select)
  end
end

---@param bufnr? integer
---@param source_id? string
---Fetch every result row for a source and present it through the configured picker.
function M.open_results(bufnr, source_id)
  bufnr = bufnr or vim.api.nvim_get_current_buf()
  local function open(id)
    if source_id and id ~= source_id then return end
    local state = coordinator.state(bufnr)
    local root = state and state.root
    local view_name = coordinator.result_view(bufnr, id) or "table"
    if not root then
      warn("Base source is no longer available")
      return
    end
    coordinator.fetch_rows(bufnr, id, function(error, rows)
      if error then
        warn(error.message or "could not fetch results")
        return
      end
      local entries = {}
      for _, row in ipairs(rows or {}) do
        local visible = table.concat(vim.tbl_map(function(cell) return cell.text or "" end, row.cells or {}), " ")
        local display = table.concat({ row.display_name or row.path, row.path, visible }, "  ")
        entries[#entries + 1] = {
          label = display,
          filename = vim.fs.joinpath(root, row.path),
          row = row,
        }
      end
      picker.pick(entries, { title = "Base results · " .. view_name }, function(entry)
        local ok, obsidian = pcall(require, "obsidian")
        if not ok then
          warn("obsidian.nvim is unavailable")
          return
        end
        obsidian.api.open_note({ filename = entry.filename })
      end)
    end)
  end
  if source_id then
    for _, choice in ipairs(coordinator.sources(bufnr)) do
      if choice.source_id == source_id then
        open(choice.source_id)
        return
      end
    end
    warn("Base source is no longer available")
  else
    choose_source(bufnr, open)
  end
end

return M
