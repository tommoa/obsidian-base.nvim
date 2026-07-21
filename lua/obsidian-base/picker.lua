-- Presents Bases choices through obsidian.nvim's configured picker.
local M = {}

---@class ObsidianBasesPickerItem
---@field label string
---@field filename? string

---Use Snacks directly when it is obsidian.nvim's configured backend. The
---current obsidian.nvim adapter forces Snacks' `default` layout, so going
---through the provider here preserves the user's global layout configuration.
---@return table?
local function snacks_picker()
  local state = rawget(_G, "Obsidian")
  local name = state and state.opts and state.opts.picker and state.opts.picker.name
  name = type(name) == "string" and name:lower() or nil
  if name ~= "snacks.picker" and name ~= "snacks.pick" then return nil end
  local ok, picker = pcall(require, "snacks.picker")
  return ok and picker or nil
end

---@param items ObsidianBasesPickerItem[]
---@param opts { title: string }
---@param callback fun(item: ObsidianBasesPickerItem)
function M.pick(items, opts, callback)
  local snacks = snacks_picker()
  if snacks then
    local preview = vim.iter(items):any(function(item) return item.filename ~= nil end)
    local entries = vim.tbl_map(function(item)
      return { text = item.label, file = item.filename, value = item }
    end, items)
    snacks.pick({
      title = opts.title,
      items = entries,
      format = "text",
      layout = { preview = preview },
      confirm = function(active, entry)
        active:close()
        if entry then callback(entry.value) end
      end,
    })
    return
  end

  local state = rawget(_G, "Obsidian")
  if state and state.picker and type(state.picker.pick) == "function" then
    local entries = vim.tbl_map(function(item)
      return {
        display = item.label,
        ordinal = item.label,
        filename = item.filename,
        user_data = item,
      }
    end, items)
    state.picker.pick(entries, {
      prompt_title = opts.title,
      callback = function(entry)
        if entry and entry.user_data then callback(entry.user_data) end
      end,
    })
    return
  end

  vim.ui.select(items, {
    prompt = opts.title,
    format_item = function(item) return item.label end,
  }, function(item)
    if item then callback(item) end
  end)
end

return M
