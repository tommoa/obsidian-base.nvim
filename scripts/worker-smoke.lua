-- Exercises the real JSON-lines worker, source discovery, and fold-expression
-- presenter without loading the user's complete Neovim configuration.
vim.opt.runtimepath:append(vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":h:h"))

local bases = require("obsidian-base")
local coordinator = require("obsidian-base.coordinator")
bases.setup()

local script = debug.getinfo(1, "S").source:sub(2)
local vault = vim.fs.joinpath(vim.fn.fnamemodify(script, ":h:h"), "fixtures", "vault")
local base = vim.fs.joinpath(vault, "Bases", "Daily log tasks.base")
vim.cmd.edit(vim.fn.fnameescape(base))
local bufnr = vim.api.nvim_get_current_buf()
vim.bo[bufnr].filetype = "obsidian_base"
vim.wo.foldmethod = "expr"
vim.wo.foldexpr = "v:lua.require'obsidian-base'.foldexpr(v:lnum)"
vim.wo.foldlevel = 99
local original_foldtext = vim.wo.foldtext
bases.refresh(bufnr)

assert(vim.wait(5000, function()
  local state = coordinator.inspect(bufnr)
  return state and state.sources[1] and (state.sources[1].result or state.sources[1].error)
end, 25), "Bases worker query timed out")

local state = assert(coordinator.inspect(bufnr))
assert(state.sources[1].result, vim.inspect(state.sources[1].error))
assert(state.sources[1].result.view_count > 0, "expected fixture Base results")
local source_id = state.sources[1].id
local views = coordinator.views(bufnr, source_id)
assert(#views == 2 and views[1].name == "Personal" and views[2].name == "Work",
  "view catalog did not come from the worker")
assert(coordinator.select_view(bufnr, source_id, "Work"), "worker-provided view should be selectable")
assert(vim.wait(5000, function()
  local current = coordinator.inspect(bufnr)
  return current and current.sources[1] and current.sources[1].result
    and current.sources[1].result.view.name == "Work"
end, 25), "selected worker view did not refresh")
assert(bases.foldexpr(1, { level = 2 }) == ">2", "fold level override was ignored")
assert(not pcall(bases.foldexpr, 1, { level = 0 }), "invalid fold level should fail")
assert(vim.fn.foldclosed(1) == -1, "Base folds should respect the user's foldlevel")
assert(vim.fn.foldlevel(1) == 1, "expected a Base fold from the fold expression")
vim.cmd("normal! zc")
assert(vim.fn.foldclosed(1) == 1, "fold expression should close the complete Base")
vim.cmd("normal! zo")
bases.refresh(bufnr)
assert(vim.wait(5000, function()
  local current = coordinator.inspect(bufnr)
  return current and current.sources[1] and current.sources[1].result
end, 25), "Bases worker refresh timed out")
assert(vim.fn.foldlevel(1) == 1, "refresh must not nest Base folds")
bases.toggle(bufnr)
assert(vim.fn.foldlevel(1) == 0, "toggle must remove Base folds")
assert(vim.wo.foldtext == original_foldtext, "Bases must not change the window foldtext")
bases.toggle(bufnr)
local previews = vim.api.nvim_buf_get_extmarks(
  0,
  require("obsidian-base.presenter").preview_namespace(),
  0,
  -1,
  { details = true }
)
assert(#previews == 1 and previews[1][4].virt_lines, "expected a persistent virtual table preview")

local daily = vim.fs.joinpath(vault, "Calendar", "Daily", "2026-07-15.md")
vim.cmd.edit(vim.fn.fnameescape(daily))
local daily_bufnr = vim.api.nvim_get_current_buf()
vim.bo[daily_bufnr].filetype = "markdown"
bases.refresh(daily_bufnr)
assert(vim.wait(5000, function()
  local current = coordinator.inspect(daily_bufnr)
  return current and current.sources[1] and current.sources[1].result
end, 25), "second Bases buffer query timed out")
local daily_state = coordinator.inspect(daily_bufnr)
local daily_source = daily_state.sources[1]
local previous_result_id = daily_source.result.result_id
vim.api.nvim_buf_set_lines(daily_bufnr, -1, -1, false, { "" })
local embed_error, embed_result
coordinator.query_embed({
  bufnr = daily_bufnr,
  workspace_root = vault,
  host_path = daily,
  source_path = "Bases/Daily log tasks.base",
  view_name = "Work",
}, function(error, result)
  embed_error, embed_result = error, result
end)
assert(vim.wait(5000, function() return embed_error ~= nil or embed_result ~= nil end, 25),
  "embed-driven overlay query timed out")
assert(not embed_error, "embed-driven overlay query failed: " .. vim.inspect(embed_error))
assert(vim.wait(5000, function()
  local current = coordinator.inspect(daily_bufnr)
  local source = current and current.sources[1]
  return source and source.result and source.result.result_id ~= previous_result_id
end, 25), "embed-driven overlay did not refresh fenced Base previews")
local embed_refreshed = coordinator.inspect(daily_bufnr).sources[1]
local embed_fetch_error, embed_rows
coordinator.fetch_rows(daily_bufnr, embed_refreshed.id, function(error, rows)
  embed_fetch_error, embed_rows = error, rows
end)
assert(vim.wait(1000, function() return embed_fetch_error ~= nil or embed_rows ~= nil end, 25),
  "embed-refreshed result fetch timed out")
assert(not embed_fetch_error and #embed_rows > 0,
  "embed-refreshed result id was invalid: " .. vim.inspect(embed_fetch_error))
previous_result_id = embed_refreshed.result.result_id
local unchanged_generation = coordinator.inspect(daily_bufnr).generation
embed_error, embed_result = nil, nil
coordinator.query_embed({
  bufnr = daily_bufnr,
  workspace_root = vault,
  host_path = daily,
  source_path = "Bases/Daily log tasks.base",
  view_name = "Work",
}, function(error, result)
  embed_error, embed_result = error, result
end)
assert(vim.wait(5000, function() return embed_error ~= nil or embed_result ~= nil end, 25),
  "unchanged embed overlay query timed out")
assert(not embed_error and coordinator.inspect(daily_bufnr).generation == unchanged_generation,
  "unchanged embed overlay refreshed fenced Base previews")
local base_lines = vim.api.nvim_buf_get_lines(bufnr, 0, -1, false)
local previous_base_result_id = coordinator.inspect(bufnr).sources[1].result.result_id
-- Leave an invalid unsaved Base in the peer buffer. The next daily overlay
-- event must re-query its last explicit snapshot rather than publishing this.
vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, { "views:", "  - type: table", "    filters: [" })
vim.api.nvim_buf_set_lines(daily_bufnr, -1, -1, false, { "" })
embed_error, embed_result = nil, nil
coordinator.query_embed({
  bufnr = daily_bufnr,
  workspace_root = vault,
  host_path = daily,
  source_path = "Bases/Daily log tasks.base",
  view_name = "Work",
}, function(error, result)
  embed_error, embed_result = error, result
end)
assert(vim.wait(5000, function() return embed_error ~= nil or embed_result ~= nil end, 25),
  "peer-refresh overlay query timed out")
assert(not embed_error, "peer-refresh overlay query failed: " .. vim.inspect(embed_error))
assert(vim.wait(5000, function()
  local current = coordinator.inspect(daily_bufnr)
  local source = current and current.sources[1]
  return source and (source.error or (source.result and source.result.result_id ~= previous_result_id))
end, 25), "peer-refresh overlay did not update fenced Base previews")
local daily_refreshed = coordinator.inspect(daily_bufnr).sources[1]
assert(daily_refreshed.result, "peer-refresh overlay query failed: " .. vim.inspect(daily_refreshed.error))
previous_result_id = daily_refreshed.result.result_id
assert(vim.wait(5000, function()
  local current = coordinator.inspect(bufnr)
  local source = current and current.sources[1]
  return source and (source.error or (source.result and source.result.result_id ~= previous_base_result_id))
end, 25), "peer index event did not re-query the cached Base snapshot")
local peer_refreshed = coordinator.inspect(bufnr).sources[1]
assert(peer_refreshed.result,
  "peer index event published the dirty Base buffer: " .. vim.inspect(peer_refreshed.error))
vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, base_lines)
vim.api.nvim_buf_set_lines(bufnr, -1, -1, false, { "" })
bases.refresh(bufnr)
assert(vim.wait(5000, function()
  local current = coordinator.inspect(daily_bufnr)
  local source = current and current.sources[1]
  return source and source.result and source.result.result_id ~= previous_result_id
end, 25), "overlay change did not refresh the peer Bases buffer")
local fetch_error, fetched_rows
local refreshed = coordinator.inspect(daily_bufnr).sources[1]
coordinator.fetch_rows(daily_bufnr, refreshed.id, function(error, rows)
  fetch_error, fetched_rows = error, rows
end)
assert(vim.wait(1000, function() return fetch_error ~= nil or fetched_rows ~= nil end, 25),
  "peer result fetch timed out")
assert(not fetch_error and #fetched_rows > 0, "peer result id was invalidated: " .. vim.inspect(fetch_error))
vim.api.nvim_set_current_buf(bufnr)

-- A filename change is also a source/workspace identity change. Exercise both
-- the same-vault and cross-vault paths through Neovim's real BufFilePost event.
local rename_root = vim.fn.tempname()
local first_vault = vim.fs.joinpath(rename_root, "first")
local second_vault = vim.fs.joinpath(rename_root, "second")
vim.fn.mkdir(vim.fs.joinpath(first_vault, ".obsidian"), "p")
vim.fn.mkdir(vim.fs.joinpath(second_vault, ".obsidian"), "p")
first_vault = vim.uv.fs_realpath(first_vault) or first_vault
second_vault = vim.uv.fs_realpath(second_vault) or second_vault
local old_base = vim.fs.joinpath(first_vault, "Old.base")
local renamed_base = vim.fs.joinpath(first_vault, "Renamed.base")
local moved_base = vim.fs.joinpath(second_vault, "Moved.base")
local recovery_base = vim.fs.joinpath(first_vault, "Recovery.base")
local empty_name_base = vim.fs.joinpath(first_vault, "Empty name.base")
vim.fn.writefile({
  "views:",
  "  - type: table",
  '    name: ""',
  "    order: []",
}, empty_name_base)
vim.cmd.edit(vim.fn.fnameescape(empty_name_base))
local empty_name_bufnr = vim.api.nvim_get_current_buf()
vim.bo[empty_name_bufnr].filetype = "obsidian_base"
bases.refresh(empty_name_bufnr)
assert(vim.wait(5000, function()
  local current = coordinator.inspect(empty_name_bufnr)
  return current and current.sources[1] and current.sources[1].result
end, 25), "empty-name view failed the worker protocol contract")
assert(coordinator.inspect(empty_name_bufnr).sources[1].result.view.name == "table",
  "empty-name view did not use the table fallback")
vim.fn.writefile({
  "views:",
  "  - type: table",
  "    name: Broken",
  "    filters: file.name ==",
  "    order: []",
  "  - type: table",
  "    name: Good",
  "    order: []",
}, recovery_base)
vim.cmd.edit(vim.fn.fnameescape(recovery_base))
local recovery_bufnr = vim.api.nvim_get_current_buf()
vim.bo[recovery_bufnr].filetype = "obsidian_base"
bases.refresh(recovery_bufnr)
assert(vim.wait(5000, function()
  local current = coordinator.inspect(recovery_bufnr)
  return current and current.sources[1] and current.sources[1].error
end, 25), "invalid default view did not return an error")
local recovery_source = coordinator.inspect(recovery_bufnr).sources[1]
local recovery_views = coordinator.views(recovery_bufnr, recovery_source.id)
assert(#recovery_views == 2 and recovery_views[1].name == "Broken" and recovery_views[2].name == "Good",
  "query error did not preserve the worker view catalog")
assert(coordinator.select_view(recovery_bufnr, recovery_source.id, "Good"),
  "valid recovery view was not selectable")
assert(vim.wait(5000, function()
  local current = coordinator.inspect(recovery_bufnr)
  return current and current.sources[1] and current.sources[1].result
    and current.sources[1].result.view.name == "Good"
end, 25), "valid view did not recover from the default view error")
vim.fn.writefile({
  "views:",
  "  - type: table",
  "    name: Table",
  "    order: [file.name]",
}, old_base)
vim.cmd.edit(vim.fn.fnameescape(old_base))
local rename_bufnr = vim.api.nvim_get_current_buf()
vim.bo[rename_bufnr].filetype = "obsidian_base"
bases.refresh(rename_bufnr)
assert(vim.wait(5000, function()
  local current = coordinator.inspect(rename_bufnr)
  return current and current.sources[1] and current.sources[1].result
end, 25), "rename fixture did not produce initial results")
local first_workspace = coordinator.state(rename_bufnr).workspace

vim.cmd.saveas({ args = { renamed_base }, bang = true })
assert(vim.wait(5000, function()
  local current = coordinator.state(rename_bufnr)
  return current and current.path == "Renamed.base" and current.sources[1]
    and current.sources[1].id == "file:Renamed.base" and current.sources[1].result
end, 25), "same-vault rename retained stale Base state: " .. vim.inspect(coordinator.inspect(rename_bufnr)))
assert(first_workspace.buffers[rename_bufnr], "same-vault rename lost workspace membership")
assert(not first_workspace.overlays["Old.base"],
  "same-vault rename retained the old overlay: " .. vim.inspect(first_workspace.overlays))
local renamed_error, renamed_rows
local renamed_source = coordinator.inspect(rename_bufnr).sources[1]
coordinator.fetch_rows(rename_bufnr, renamed_source.id, function(error, rows)
  renamed_error, renamed_rows = error, rows
end)
assert(vim.wait(1000, function() return renamed_error ~= nil or renamed_rows ~= nil end, 25),
  "post-save result fetch timed out")
assert(not renamed_error, "post-save result id was invalidated: " .. vim.inspect(renamed_error))

vim.cmd.saveas({ args = { moved_base }, bang = true })
assert(vim.wait(5000, function()
  local current = coordinator.state(rename_bufnr)
  return current and current.root == second_vault and current.path == "Moved.base" and current.sources[1]
    and current.sources[1].id == "file:Moved.base" and current.sources[1].result
end, 25), "cross-vault rename retained stale Base state: " .. vim.inspect(coordinator.inspect(rename_bufnr)))
assert(not first_workspace.buffers[rename_bufnr], "cross-vault rename retained old workspace membership")
assert(not first_workspace.overlays["Renamed.base"], "cross-vault rename retained the old overlay")
vim.cmd.bwipeout({ bang = true })
vim.fn.delete(rename_root, "rf")

print("obsidian-base worker smoke passed")
