-- Covers the public embed-provider boundary with a real Bases worker query.
local test = dofile(vim.fn.expand("~/.config/nvim/lua/tom/obsidian_embeds/scripts/obsidian-embeds-testlib.lua"))
test.setup_runtime()

-- Providers are asynchronous resources: their cancellation must suppress a
-- late completion before it reaches a render generation.
local providers = require("tom.obsidian_embeds.providers")
local order = {}
local unregister_first = providers.register({
  name = "embed-order-first",
  match = function(ref) return ref.target == "ordered.base" end,
  render_async = function(_, _, done) order[#order + 1] = "first"; done({ project = function() return {} end }) end,
})
local unregister_second = providers.register({
  name = "embed-order-second",
  match = function(ref) return ref.target == "ordered.base" end,
  render_async = function(_, _, done) order[#order + 1] = "second"; done({ project = function() return {} end }) end,
})
local ordered = assert(providers.match({ target = "ordered.base" }, {}))
providers.render_async(ordered, { target = "ordered.base" }, {}, function() end)
test.assert_true(order[1] == "first", "first registered matching provider should win")
unregister_first()
local after_unregister = assert(providers.match({ target = "ordered.base" }, {}))
providers.render_async(after_unregister, { target = "ordered.base" }, {}, function() end)
test.assert_true(order[2] == "second", "unregister should reveal the next matching provider")
unregister_second()

local cancelled, completed, finish = false, false, nil
local unregister = providers.register({
  name = "embed-cancellation-smoke",
  match = function(ref) return ref.target == "slow.base" end,
  render_async = function(_, _, done)
    finish = done
    return function() cancelled = true end
  end,
})
local provider = assert(providers.match({ target = "slow.base" }, {}))
local owner = {}
providers.render_async(provider, { target = "slow.base" }, {}, function() completed = true end, owner)
providers.cancel_owner(owner)
finish({ project = function() return {} end })
test.assert_true(cancelled and not completed, "provider cancellation must reject late completion")
unregister()

local root = vim.fn.tempname()
vim.fn.mkdir(root .. "/.obsidian", "p")
vim.fn.mkdir(root .. "/Bases", "p")
vim.fn.mkdir(root .. "/Items", "p")
test.write_file(root .. "/Items/Task.md", { "---", "status: open", "---", "Task body" })
test.write_file(root .. "/Bases/Tasks.base", {
  "views:",
  "  - name: Overview",
  "    type: table",
  "    order: [file.name]",
  "  - name: Work",
  "    type: table",
  "    order: [status, file.name]",
})
test.write_file(root .. "/Host.md", { "![[Bases/Tasks.base#Work]]" })

require("obsidian").setup({
  legacy_commands = false,
  workspaces = { { name = "bases-embed", path = root } },
  picker = { name = false },
  ui = { enable = false },
})

vim.cmd.edit(vim.fn.fnameescape(root .. "/Host.md"))
local bufnr = vim.api.nvim_get_current_buf()
vim.b[bufnr].obsidian_buffer = true

local bases = require("obsidian-base")
bases.setup()
local embeds = require("tom.obsidian_embeds")
embeds.setup()
test.wait_complete(embeds, bufnr, 10000)

local rendered = test.rendered_text(bufnr, require("tom.obsidian_embeds.config").ns_id)
test.assert_contains(rendered, "status", "named Base view should render its selected columns")
test.assert_contains(rendered, "Task", "Base embed should render matching vault rows")

test.assert_true(embeds.inspect(bufnr) ~= nil, "Base embed inspection should return a snapshot")
print("obsidian-base embed smoke passed")
