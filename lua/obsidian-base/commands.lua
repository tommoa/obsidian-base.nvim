-- Integrates Obsidian Bases actions with obsidian.nvim's command and LSP action APIs.
local M = {}
local installed = false

---Return the buffer that should receive an Obsidian command.
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
        { "bases_view",    "Select Base view",      function(bases, bufnr, source_id, row) bases.select_view(bufnr, source_id, row) end },
        { "bases_results", "Open Base results",     function(bases, bufnr, source_id, row) bases.open_results(bufnr, source_id, row) end },
        { "bases_inspect", "Inspect Base preview",  function(bases, bufnr) bases.inspect(bufnr) end },
    }
    for _, action in ipairs(actions) do
        obsidian.code_action.add({
            name = action[1],
            title = action[2],
            cond = function(_, params)
                local bufnr = vim.uri_to_bufnr(params.textDocument.uri)
                local coordinator = require("obsidian-base.coordinator")
                return coordinator.code_actions_available(bufnr)
            end,
            -- obsidian.nvim resolves arguments for each request before its
            -- scheduled command runs, so overlapping requests keep their context.
            arguments = function(_, params)
                local bufnr = vim.uri_to_bufnr(params.textDocument.uri)
                local row = params.range.start.line
                local source_id = require("obsidian-base.coordinator").source_at_cursor(bufnr, row)
                -- Keep the row in the third slot when no source is under the cursor.
                return { bufnr, source_id or false, row }
            end,
            fn = function(bufnr, source_id, row)
                if not bufnr or not vim.api.nvim_buf_is_valid(bufnr) then
                    vim.notify("Obsidian Bases: code-action buffer is no longer available", vim.log.levels.WARN)
                    return
                end
                action[3](require("obsidian-base"), bufnr, source_id ~= false and source_id or nil, row)
            end,
        })
    end
    return true
end

return M
