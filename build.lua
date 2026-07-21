-- lazy.nvim runs build.lua without loading the plugin, so load the installer
-- directly from this checkout instead of relying on runtimepath or module cache state.
local source = debug.getinfo(1, "S").source:sub(2)
local root = vim.fs.dirname(vim.fn.fnamemodify(source, ":p"))
local installer_chunk, load_error = loadfile(vim.fs.joinpath(root, "lua", "obsidian-base", "installer.lua"))
assert(installer_chunk, load_error)
local installer = installer_chunk()

local done, install_error = false, nil
local cancel = installer.install({ strategy = "auto" }, function(error)
    install_error, done = error, true
end)
if not vim.wait(600000, function() return done end, 20) then
    cancel()
    error("worker installation timed out")
end
if install_error then error(install_error) end
