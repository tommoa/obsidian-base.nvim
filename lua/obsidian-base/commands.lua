-- Integrates Obsidian Bases actions with obsidian.nvim's command and LSP action APIs.
local M = {}
local installed = false
local code_action_bufnr

---Return the buffer that should receive an Obsidian command or code action.
---@return integer bufnr
local function current_buf()
    return vim.api.nvim_get_current_buf()
end

---Register the `:Obsidian bases` command and the associated code actions once.
---@return boolean registered Whether obsidian.nvim was available and registration succeeded.
function M.setup()
    if installed then return true end
    if not _G.Obsidian then return false end
    local ok, obsidian = pcall(require, "obsidian")
    if not ok then return false end
    installed = true
    obsidian.register_command("bases", {
        nargs = "?",
        note_action = false,
        complete = function(arg_lead)
            return vim.tbl_filter(function(item) return vim.startswith(item, arg_lead) end,
                { "toggle", "refresh", "view", "results", "inspect" })
        end,
        func = function(data)
            local bases = require("obsidian-base")
            local subcommand = data.fargs[1] or "toggle"
            if subcommand == "toggle" then
                bases.toggle(current_buf())
            elseif subcommand == "refresh" then
                bases.refresh(current_buf())
            elseif subcommand == "view" then
                bases.select_view(current_buf())
            elseif subcommand == "results" then
                bases.open_results(current_buf())
            elseif subcommand == "inspect" then
                bases.inspect(current_buf())
            else
                vim.notify("Usage: Obsidian bases [toggle|refresh|view|results|inspect]", vim.log.levels.ERROR)
            end
        end,
    })
    local actions = {
        { "bases_toggle",  "Toggle Base previews",  function(bases, bufnr) bases.toggle(bufnr) end },
        { "bases_refresh", "Refresh Base previews", function(bases, bufnr) bases.refresh(bufnr) end },
        { "bases_view",    "Select Base view",      function(bases, bufnr) bases.select_view(bufnr) end },
        { "bases_results", "Open Base results",     function(bases, bufnr) bases.open_results(bufnr) end },
        { "bases_inspect", "Inspect Base preview",  function(bases, bufnr) bases.inspect(bufnr) end },
    }
    for _, action in ipairs(actions) do
        obsidian.code_action.add({
            name = action[1],
            title = action[2],
            cond = function()
                local bufnr = current_buf()
                local coordinator = require("obsidian-base.coordinator")
                if not coordinator.code_actions_available(bufnr) then return false end
                code_action_bufnr = bufnr
                coordinator.remember_code_action_context(bufnr)
                return true
            end,
            fn = function()
                local bufnr = code_action_bufnr
                if not bufnr or not vim.api.nvim_buf_is_valid(bufnr) then
                    vim.notify("Obsidian Bases: code-action buffer is no longer available", vim.log.levels.WARN)
                    return
                end
                action[3](require("obsidian-base"), bufnr)
            end,
        })
    end
    return true
end

return M
