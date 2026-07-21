-- Adapts the public Bases surface to the public Obsidian embeds provider API.
local coordinator = require("obsidian-base.coordinator")
local presenter = require("obsidian-base.presenter")

local M = {}
local registered = false
local subscribers = {}
local cleanup_installed = false

local function unsubscribe(key)
  local subscriber = subscribers[key]
  if not subscriber then return end
  subscriber.unsubscribe()
  subscribers[key] = nil
end

local function unsubscribe_buffer(bufnr)
  local keys = {}
  for key, subscriber in pairs(subscribers) do
    if subscriber.bufnr == bufnr then keys[#keys + 1] = key end
  end
  for _, key in ipairs(keys) do unsubscribe(key) end
end

local function ensure_cleanup()
  if cleanup_installed then return end
  cleanup_installed = true
  local group = vim.api.nvim_create_augroup("obsidian-base-embed-subscribers", { clear = true })
  vim.api.nvim_create_autocmd("BufWipeout", {
    group = group,
    callback = function(args) unsubscribe_buffer(args.buf) end,
  })
end

local function contained_relative(root, path)
  local relative = vim.fs.relpath(root, path)
  if not relative or relative == ".." or vim.startswith(relative, "../") then return nil end
  return relative:gsub("\\", "/")
end

local function source_path(ctx, ref)
  local target = vim.uri_decode(ref.target or "")
  if target == "" or target:lower():sub(-5) ~= ".base" then return nil end
  if target:sub(1, 1) == "/" then return nil end
  local root = vim.uv.fs_realpath(ctx.workspace_root) or ctx.workspace_root
  local base_dir = ctx.base_dir or root
  local relative_candidate = vim.fs.normalize(vim.fs.joinpath(base_dir, target))
  local root_candidate = vim.fs.normalize(vim.fs.joinpath(root, target))
  local candidate = vim.uv.fs_stat(relative_candidate) and relative_candidate or root_candidate
  return contained_relative(root, candidate), candidate
end

local function subscribe(ctx)
  local root = vim.uv.fs_realpath(ctx.workspace_root) or ctx.workspace_root
  local key = root .. "\0" .. tostring(ctx.bufnr)
  if subscribers[key] then return end
  ensure_cleanup()
  subscribers[key] = {
    bufnr = ctx.bufnr,
    unsubscribe = coordinator.subscribe_index(root, function()
    if not vim.api.nvim_buf_is_valid(ctx.bufnr) then
      unsubscribe(key)
      return
    end
    vim.schedule(function()
      if vim.api.nvim_buf_is_valid(ctx.bufnr) then require("tom.obsidian_embeds").refresh(ctx.bufnr, true) end
    end)
    end),
  }
end

function M.register()
  if registered then return true end
  local ok, embeds = pcall(require, "tom.obsidian_embeds")
  if not ok then return false end
  embeds.register_provider({
    name = "obsidian-base",
    match = function(ref)
      return type(ref.target) == "string" and vim.uri_decode(ref.target):lower():sub(-5) == ".base"
    end,
    render_async = function(ref, ctx, done)
      local source, absolute = source_path(ctx, ref)
      if not source then
        done(nil, "Base embed must resolve to a .base file inside this vault")
        return
      end
      if ref.block then
        done(nil, "Base embeds support a view selector, not a block selector")
        return
      end
      return coordinator.query_embed({
        bufnr = ctx.bufnr,
        workspace_root = ctx.workspace_root,
        host_path = ctx.host_path,
        source_path = source,
        view_name = ref.anchor,
      }, function(error, result)
        if error then done(nil, error.message or error.code); return end
        subscribe(ctx)
        local immutable = vim.deepcopy(result)
        done({
          dependencies = { absolute },
          project = function(width)
            return presenter.project_result(immutable, math.max(1, width), "")
          end,
        })
      end)
    end,
  })
  registered = true
  return true
end

return M
