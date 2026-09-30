-- Renders Base query results as virtual lines and exposes an opt-in fold expression.
local config = require("obsidian-base.config")

local M = {}

---@type integer
local namespace = vim.api.nvim_create_namespace("obsidian-base-preview")
---@type integer
local source_namespace = vim.api.nvim_create_namespace("obsidian-base-sources")
local state_for
-- Cache ownership follows coordinator buffer state, including detach and wipeout.
local fold_cache = setmetatable({}, { __mode = "k" })

---Measure text using Neovim's display-cell width rules.
---@param text string
---@return integer
local function display_width(text)
  return vim.fn.strdisplaywidth(text)
end

---Truncate display text to a cell width while preserving multibyte characters.
---@param text unknown
---@param width integer
---@return string
local function truncate(text, width)
  text = tostring(text or "")
  if width <= 0 then return "" end
  if display_width(text) <= width then return text end
  if width == 1 then return "…" end
  local out, index = "", 0
  -- The input is immutable; count it once instead of rescanning it per character.
  local character_count = vim.fn.strchars(text)
  while index < character_count do
    local character = vim.fn.strcharpart(text, index, 1)
    if display_width(out .. character .. "…") > width then break end
    out = out .. character
    index = index + 1
  end
  return out .. "…"
end

---Create the virtual-line chunk format expected by `nvim_buf_set_extmark`.
---@param text string
---@param group string
---@return table[]
local function chunks(text, group)
  return { { text, group } }
end

---Resolve a source's current line range from its stable extmarks.
---@param bufnr integer
---@param source ObsidianBasesSource
---@return integer?, integer?
local function source_range(bufnr, source)
  local start = vim.api.nvim_buf_get_extmark_by_id(bufnr, source_namespace, source.start_mark, {})[1]
  local finish = vim.api.nvim_buf_get_extmark_by_id(bufnr, source_namespace, source.end_mark, {})[1]
  if start == nil or finish == nil then return nil end
  return start, finish
end

---Translate an extmark range into the inclusive range used by fold expressions.
---@param source ObsidianBasesSource
---@param start integer?
---@param finish integer?
---@return integer?, integer?
local function source_fold_range(source, start, finish)
  if source.end_is_eof then finish = finish - 1 end
  return start, finish
end

---Snapshot moving anchors until text changes, sources are replaced, or refresh_foldexpr runs.
---@param bufnr integer
---@param buffer ObsidianBasesBuffer
---@return table[] Inclusive zero-based fold ranges in source order.
local function fold_ranges(bufnr, buffer)
  local tick = vim.api.nvim_buf_get_changedtick(bufnr)
  local cached = fold_cache[buffer]
  -- Detach can replace sources before a later refresh, without changing the text.
  if cached and cached.tick == tick and cached.sources == buffer.sources then return cached.ranges end
  local ranges = {}
  for _, source in ipairs(buffer.sources) do
    local start, finish = source_range(bufnr, source)
    if start and finish then
      start, finish = source_fold_range(source, start, finish)
      if finish > start then ranges[#ranges + 1] = { start, finish } end
    end
  end
  fold_cache[buffer] = { tick = tick, sources = buffer.sources, ranges = ranges }
  return ranges
end

---Find the first source covering a row without scanning sources per fold call.
---@param ranges table[]
---@param row integer Zero-based buffer row.
---@return table?
local function fold_range_at(ranges, row)
  -- Discovery orders sources by row. Starts share left gravity and ends share
  -- right gravity, so edits preserve endpoint order even when ranges overlap.
  -- The first end at or after this row preserves the original first-match rule.
  local low, high = 1, #ranges
  while low <= high do
    local middle = math.floor((low + high) / 2)
    if ranges[middle][2] < row then low = middle + 1 else high = middle - 1 end
  end
  local range = ranges[low]
  if range and range[1] <= row then return range end
end

---Format a table result into width-aware highlighted virtual lines.
---@param result ObsidianBasesTableResult
---@param width integer
---@param opts ObsidianBasesConfig
---@return table[]
local function table_lines(result, width, opts, rail)
  rail = rail == nil and opts.presentation.rail or rail
  local columns = result.columns or {}
  if #columns == 0 then return { chunks(rail .. "No columns", opts.presentation.highlights.warning) } end
  local rail_width = display_width(rail)
  local available = math.max(#columns * 4, width - rail_width - (#columns + 1) * 3)
  local widths, natural = {}, 0
  for index, column in ipairs(columns) do
    local maximum = display_width(column.label)
    for _, row in ipairs(result.preview_rows or {}) do
      maximum = math.max(maximum, display_width((row.cells[index] or {}).text or ""))
    end
    widths[index] = math.max(3, maximum)
    natural = natural + widths[index]
  end
  while natural > available do
    local largest, largest_index = 0, 1
    for index, value in ipairs(widths) do
      if value > largest then largest, largest_index = value, index end
    end
    if largest <= 3 then break end
    widths[largest_index] = largest - 1
    natural = natural - 1
  end

  ---Render one header or data row using the calculated column widths.
  ---@param values string[]
  ---@param group string
  ---@return table[]
  local function line(values, group)
    local parts = { rail, "│ " }
    for index, value in ipairs(values) do
      local displayed = truncate(value, widths[index])
      parts[#parts + 1] = displayed
      local padding = widths[index] - display_width(displayed)
      parts[#parts + 1] = string.rep(" ", padding) .. " │ "
    end
    return chunks(table.concat(parts), group)
  end
  local border = rail .. "├" .. table.concat(vim.tbl_map(function(value)
    return string.rep("─", value + 2)
  end, widths), "┼") .. "┤"
  local header = {}
  for index, column in ipairs(columns) do header[index] = column.label end
  local lines = { line(header, opts.presentation.highlights.header), chunks(border, opts.presentation.highlights.border) }
  for _, row in ipairs(result.preview_rows or {}) do
    local cells = {}
    for index, cell in ipairs(row.cells or {}) do cells[index] = cell.text end
    lines[#lines + 1] = line(cells, opts.presentation.highlights.text)
  end
  if #(result.preview_rows or {}) == 0 then
    lines[#lines + 1] = chunks(rail .. "No results", opts.presentation.highlights.text)
  end
  if result.view_count and result.preview_count and result.view_count > result.preview_count then
    lines[#lines + 1] = chunks(
      rail .. string.format("Showing %d of %d results", result.preview_count, result.view_count),
      opts.presentation.highlights.warning
    )
  end
  return lines
end

---Project a semantic Base table without placing extmarks or touching buffers.
---@param result ObsidianBasesTableResult
---@param width integer
---@param rail? string
---@return table[]
function M.project_result(result, width, rail)
  return table_lines(result, width, config.get(), rail)
end

---Choose loading, error, or table virtual lines for a source's current state.
---@param source ObsidianBasesSource
---@param width integer
---@return table[]
local function projection(source, width)
  local opts = config.get()
  if source.error then
    return { chunks(opts.presentation.rail .. "Base error: " .. source.error.message, opts.presentation.highlights.warning) }
  end
  if source.loading then
    return { chunks(opts.presentation.rail .. "Base loading…", opts.presentation.highlights.loading) }
  end
  if source.result then return table_lines(source.result, width, opts) end
  return {}
end

---Redraw all virtual previews for an attached buffer.
---@param bufnr integer
function M.sync_buffer(bufnr)
  local buffer = state_for and state_for(bufnr)
  vim.api.nvim_buf_clear_namespace(bufnr, namespace, 0, -1)
  if not buffer or not buffer.enabled then return end
  local current = vim.api.nvim_get_current_win()
  local width = vim.api.nvim_win_is_valid(current) and vim.api.nvim_win_get_buf(current) == bufnr
      and vim.api.nvim_win_get_width(current)
    or (vim.fn.win_findbuf(bufnr)[1] and vim.api.nvim_win_get_width(vim.fn.win_findbuf(bufnr)[1]))
    or 80
  for _, source in ipairs(buffer.sources) do
    local _, finish = source_range(bufnr, source)
    if finish then
      local anchor = source.end_is_eof and finish or finish + 1
      vim.api.nvim_buf_set_extmark(bufnr, namespace, anchor, 0, {
        virt_lines = projection(source, width),
        virt_lines_above = true,
      })
    end
  end
  vim.api.nvim__redraw({ buf = bufnr, valid = true, flush = false })
end

---Remove all plugin previews belonging to a buffer.
---@param bufnr integer
function M.clear_buffer(bufnr)
  vim.api.nvim_buf_clear_namespace(bufnr, namespace, 0, -1)
end

---Return the fold level for a line in the current Base buffer.
---Outside Base sources, this preserves the caller-provided fallback level.
---@param lnum integer One-based buffer line number.
---@param fallback? fun(): integer|string
---@param base_level? integer
---@return integer|string
function M.foldexpr(lnum, fallback, base_level)
  local function fallback_level()
    if not fallback then return 0 end
    local ok, value = pcall(fallback)
    if not ok then return 0 end
    if type(value) == "number" then return value end
    return tonumber(tostring(value):match("%d+")) or 0
  end

  local bufnr = vim.api.nvim_get_current_buf()
  local buffer = state_for and state_for(bufnr)
  if buffer and buffer.enabled then
    local range = fold_range_at(fold_ranges(bufnr, buffer), lnum - 1)
    if range then
      local level = base_level or fallback_level() + 1
      if lnum == range[1] + 1 then return ">" .. level end
      if lnum == range[2] + 1 then return "<" .. level end
      return level
    end
  end
  return fallback_level()
end

---Refresh cached fold levels for windows explicitly using the Base fold expression.
---@param bufnr integer
function M.refresh_foldexpr(bufnr)
  local buffer = state_for and state_for(bufnr)
  if buffer then fold_cache[buffer] = nil end
  for _, win in ipairs(vim.fn.win_findbuf(bufnr)) do
    if vim.wo[win].foldmethod == "expr" and vim.wo[win].foldexpr:find("obsidian-base", 1, true) then
      vim.api.nvim_win_call(win, function() vim.cmd("silent! normal! zx") end)
    end
  end
end

---Provide the coordinator state lookup used while rendering previews.
---@param accessor fun(bufnr: integer): ObsidianBasesBuffer?
function M.set_state_accessor(accessor)
  state_for = accessor
end

---Return the extmark namespace that owns source anchors.
---@return integer
function M.source_namespace()
  return source_namespace
end

---Return the extmark namespace that owns virtual Base previews.
---@return integer
function M.preview_namespace()
  return namespace
end

return M
