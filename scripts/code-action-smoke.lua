-- Simulate obsidian.nvim's code-action flow: cond and arguments run at request
-- time, while fn runs after the user selects an action.
vim.opt.runtimepath:append(vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":h:h"))

local actions, calls, checked = {}, {}, {}
_G.Obsidian = {}
package.preload["obsidian"] = function()
  return {
    register_command = function() end,
    code_action = { add = function(opts) actions[opts.name] = opts end },
  }
end
package.loaded["obsidian-base.coordinator"] = {
  code_actions_available = function(bufnr)
    checked[#checked + 1] = bufnr
    return true
  end,
  source_at_cursor = function(bufnr, row) return row ~= 99 and bufnr .. ":" .. row or nil end,
}
package.loaded["obsidian-base"] = {
  toggle = function(bufnr) calls[#calls + 1] = { "toggle", bufnr } end,
  select_view = function(bufnr, source_id, row) calls[#calls + 1] = { "view", bufnr, source_id or false, row } end,
}

assert(require("obsidian-base.commands").setup())
local first = vim.api.nvim_create_buf(true, false)
local second = vim.api.nvim_create_buf(true, false)
vim.api.nvim_buf_set_name(first, "/tmp/obsidian-base-code-action-first.md")
vim.api.nvim_buf_set_name(second, "/tmp/obsidian-base-code-action-second.md")

local function request(action, bufnr, row)
  local params = {
    textDocument = { uri = vim.uri_from_bufnr(bufnr) },
    range = { start = { line = row, character = 0 } },
  }
  assert(action.cond(nil, params))
  return action.arguments and action.arguments(nil, params) or {}
end

local toggle = actions.bases_toggle
local view = actions.bases_view
vim.api.nvim_set_current_buf(first)
local first_toggle = request(toggle, first, 2)
local first_view = request(view, first, 3)
vim.api.nvim_set_current_buf(second)
local second_toggle = request(toggle, second, 4)
local second_view = request(view, second, 5)
-- The active window can already be elsewhere when obsidian.nvim handles a request.
local first_view_without_source = request(view, first, 99)
assert(vim.deep_equal(checked, { first, first, second, second, first }), vim.inspect(checked))

toggle.fn(unpack(first_toggle))
view.fn(unpack(first_view))
toggle.fn(unpack(second_toggle))
view.fn(unpack(second_view))
view.fn(unpack(first_view_without_source))

assert(vim.deep_equal(calls, {
  { "toggle", first },
  { "view", first, first .. ":3", 3 },
  { "toggle", second },
  { "view", second, second .. ":5", 5 },
  { "view", first, false, 99 },
}), vim.inspect(calls))
print("obsidian-base code action smoke passed")
