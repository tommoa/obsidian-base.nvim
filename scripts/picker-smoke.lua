-- Covers the Snacks compatibility path without loading the user's config.
vim.opt.runtimepath:append(vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":h:h"))

local request
package.preload["snacks.picker"] = function()
  return { pick = function(opts) request = opts end }
end
_G.Obsidian = { opts = { picker = { name = "snacks.picker" } } }

local selected, closed
require("obsidian-base.picker").pick({
  { label = "Result", filename = "/vault/result.md", id = "stable-id" },
}, { title = "Base results" }, function(item)
  selected = item.id
end)

assert(request, "expected Snacks picker request")
assert(request.layout.preview, "file entries should enable preview")
assert(request.layout.preset == nil, "Bases must inherit the configured Snacks layout")
request.confirm({ close = function() closed = true end }, request.items[1])
assert(closed, "confirm should close the picker")
assert(selected == "stable-id", "picker should return the immutable domain item")
print("obsidian-base picker smoke passed")
