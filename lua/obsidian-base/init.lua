-- Exposes the public Obsidian Bases API and connects configuration, rendering, and UI modules.
local config = require("obsidian-base.config")
local coordinator = require("obsidian-base.coordinator")
local inspect = require("obsidian-base.inspect")
local presenter = require("obsidian-base.presenter")
local ui = require("obsidian-base.ui")

local M = {}

---Configure direct Base buffers to use YAML filetype settings.
local function setup_base_filetype()
    local group = vim.api.nvim_create_augroup("obsidian-base-filetype", { clear = true })
    vim.api.nvim_create_autocmd("FileType", {
        group = group,
        pattern = "obsidian_base",
        callback = function(args)
            if vim.api.nvim_buf_get_name(args.buf):sub(-5) == ".base" then
                vim.bo[args.buf].filetype = "yaml"
            end
        end,
    })
    local bufnr = vim.api.nvim_get_current_buf()
    if vim.api.nvim_buf_get_name(bufnr):sub(-5) == ".base" then vim.bo[bufnr].filetype = "yaml" end
end

---Install configuration, direct-Base support, and optional obsidian.nvim integration.
---@param opts? ObsidianBasesConfig
---@return ObsidianBasesConfig
function M.setup(opts)
    local normalized = config.setup(opts)
    setup_base_filetype()
    coordinator.setup()
    coordinator.attach(vim.api.nvim_get_current_buf())
    -- Bases stays usable without embeds; registration merely makes the
    -- optional public provider available when that plugin is present.
    require("obsidian-base.embed").register()
    if not M.register_obsidian() then vim.schedule(M.register_obsidian) end
    return normalized
end

---Return the current normalized configuration.
---@return ObsidianBasesConfig
function M.config()
    return config.get()
end

---Install a managed native worker without blocking Neovim.
---@param opts? ObsidianBaseInstallOptions
---@param callback? fun(error: string?, result?: table)
---@return fun() cancel
function M.install_worker(opts, callback)
    return require("obsidian-base.installer").install(opts, callback)
end

---Enable or hide Base previews in a buffer.
---@param bufnr? integer
function M.toggle(bufnr) coordinator.toggle(bufnr or vim.api.nvim_get_current_buf()) end

---Rediscover Base sources and rerun their worker queries.
---@param bufnr? integer
function M.refresh(bufnr) coordinator.refresh(bufnr or vim.api.nvim_get_current_buf()) end

---Prompt for a Base view and refresh the selected source with it.
---@param bufnr? integer
---@param source_id? string
function M.select_view(bufnr, source_id) ui.select_view(bufnr or vim.api.nvim_get_current_buf(), source_id) end

---Open the complete cached result set for a Base source in obsidian.nvim's picker.
---@param bufnr? integer
---@param source_id? string
function M.open_results(bufnr, source_id) ui.open_results(bufnr or vim.api.nvim_get_current_buf(), source_id) end

---@class ObsidianBaseFoldOptions
---@field level? integer Absolute fold level for Base sources.

---Validate the optional fold-expression level override.
---@param opts? ObsidianBaseFoldOptions|integer
---@return integer?
local function fold_level(opts)
  if opts == nil then return nil end
  if type(opts) == "number" then opts = { level = opts } end
  if type(opts) ~= "table" then error("obsidian-base fold options must be a table or integer", 3) end
  for key in pairs(opts) do
    if type(key) ~= "string" then error("obsidian-base fold option keys must be strings", 3) end
    if key ~= "level" then error("unknown obsidian-base fold option: " .. key, 3) end
  end
  if opts.level == nil then return nil end
  if type(opts.level) ~= "number" or opts.level < 1 or opts.level ~= math.floor(opts.level) then
    error("obsidian-base fold level must be a positive integer", 3)
  end
  return opts.level
end

---Return the Base fold level for a line when used as a window `foldexpr`.
---@param lnum integer
---@param opts? ObsidianBaseFoldOptions|integer
---@return integer|string
function M.foldexpr(lnum, opts)
  local fallback = vim.api.nvim_buf_get_name(0):sub(-5) ~= ".base" and vim.treesitter.foldexpr or nil
  return presenter.foldexpr(lnum, fallback, fold_level(opts))
end

---Open a read-only snapshot of the worker, source, cache, timing, and error state.
---@param bufnr? integer
---@return table?
function M.inspect(bufnr)
    bufnr = bufnr or vim.api.nvim_get_current_buf()
    local snapshot = coordinator.inspect(bufnr)
    if not snapshot then return nil end
    local inspect_bufnr = inspect.show(snapshot)
    coordinator.inspect_worker(bufnr, function(error, worker)
        if error then
            snapshot.worker_error = error.message or snapshot.worker_error
            inspect.show(snapshot, nil, inspect_bufnr)
        else
            inspect.show(snapshot, worker, inspect_bufnr)
        end
    end)
    return snapshot
end

---Register the public obsidian.nvim command and code actions after obsidian.setup().
---@return boolean
function M.register_obsidian()
    return require("obsidian-base.commands").setup()
end

return M
