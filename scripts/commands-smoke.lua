-- Covers public command/action registration and buffer-local view selection
-- without loading the user's complete Neovim configuration.
vim.opt.runtimepath:append(vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":h:h"))

local registered, actions, picked, picker_requests = {}, {}, nil, {}
_G.Obsidian = {
  opts = { picker = { name = "test" } },
  picker = {
    pick = function(entries, opts)
      picker_requests[#picker_requests + 1] = { entries = entries, opts = opts }
    end,
  },
}
package.preload["obsidian"] = function()
  return {
    api = { open_note = function(entry) picked = entry.filename end },
    register_command = function(name, opts) registered[name] = opts end,
    code_action = { add = function(opts) actions[opts.name] = opts end },
  }
end

local bases = require("obsidian-base")
bases.setup()
assert(bases.register_obsidian(), "expected obsidian command registration")
assert(registered.bases, "expected Obsidian bases command")
assert(vim.tbl_count(actions) == 5, "expected five Bases code actions")

local script = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p")
local vault = vim.fs.joinpath(vim.fn.fnamemodify(script, ":h:h"), "fixtures", "vault")
local base = vim.fs.joinpath(vault, "Bases", "Daily log tasks.base")
vim.cmd.edit(vim.fn.fnameescape(base))
vim.bo.filetype = "obsidian_base"
bases.refresh(0)

local coordinator = require("obsidian-base.coordinator")
assert(vim.wait(5000, function()
  local state = coordinator.inspect(0)
  return state and state.sources[1] and state.sources[1].result
end, 25), "Bases worker query timed out")

local state = assert(coordinator.inspect(0))
local source = state.sources[1]
assert(source.result.view.name == "Personal", "expected first view")
local generation = state.generation
vim.api.nvim_exec_autocmds("BufEnter", { buffer = 0 })
assert(coordinator.inspect(0).generation == generation, "BufEnter must not refresh an attached buffer")

bases.select_view(0, source.id)
assert(#picker_requests == 1, "expected configured picker for view selection")
assert(picker_requests[1].opts.prompt_title == "Select Base view", "expected view picker title")
-- Refresh after the picker has captured its snapshot, then apply the stable
-- view name through the deferred callback.
coordinator.refresh(0)
local work_entry = vim.iter(picker_requests[1].entries):find(function(entry)
  return entry.user_data.name == "Work"
end)
assert(work_entry, "expected Work view picker entry")
picker_requests[1].opts.callback(work_entry)
assert(vim.wait(5000, function()
  local current = coordinator.inspect(0)
  return current.sources[1] and current.sources[1].result and current.sources[1].result.view.name == "Work"
end, 25), "view selection did not re-query")
assert(not coordinator.select_view(0, source.id, "Missing"), "stale view names must be rejected")

bases.open_results(0, source.id)
assert(vim.wait(1000, function() return #picker_requests == 2 end, 25), "result picker did not open")
assert(#picker_requests[2].entries > 0, "expected result-picker entries")
picker_requests[2].opts.callback(picker_requests[2].entries[1])
assert(picked ~= nil, "result picker did not open a row")
assert(vim.startswith(picked, vault .. "/"), "picker should open a vault file: " .. tostring(picked))

registered.bases.func({ fargs = { "refresh" } })
print("obsidian-base commands smoke passed")
