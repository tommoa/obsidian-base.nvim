-- Coordinates buffer discovery, the per-vault worker lifecycle, and preview updates.
local contract = require("obsidian-base.contract")
local config = require("obsidian-base.config")
local installer = require("obsidian-base.installer")
local presenter = require("obsidian-base.presenter")

local M = {}
---@type table<string, ObsidianBasesWorkspace>
local workspaces = {}
---@type table<integer, ObsidianBasesBuffer>
local buffers = {}
---@type table<string, integer[]>
local crash_history = {}
---@type table<string, table<integer, fun(event: table)>>
local index_subscribers = {}
local next_index_subscriber = 1
-- Autocmd callbacks read live configuration, so setup only needs to register them once.
local configured = false
-- The source query is static; only Tree-sitter parsing of the current buffer varies.
local base_fence_query
---@type table<integer, integer>
local filepost_generation = {}
local augroup = vim.api.nvim_create_augroup("obsidian-base", { clear = true })

---@class ObsidianBasesSource
---@field id string
---@field kind "inline"|"file"
---@field start_mark integer
---@field end_mark integer
---@field end_is_eof? boolean
---@field path? string
---@field text string
---@field selected_view? string
---@field available_views? ObsidianBasesTableView[]
---@field vault_name string
---@field loading boolean
---@field error? ObsidianBasesProtocolError
---@field result? ObsidianBasesTableResult

---@class ObsidianBasesWorkspace
---@field root string
---@field buffers table<integer, boolean>
---@field pending table<integer, { method: string, callback: fun(error: ObsidianBasesProtocolError?, result: table?) }>
---@field results table<string, ObsidianBasesTableResult>
---@field next_id integer
---@field generation integer
---@field stderr_tail string
---@field status "starting"|"ready"|"failed"|"stopping"
---@field overlays table<string, string>
---@field ready_waiters table[]
---@field error? string
---@field stdin? uv.uv_pipe_t
---@field stdout? uv.uv_pipe_t
---@field stdout_buffer? string
---@field stderr? uv.uv_pipe_t
---@field handle? uv.uv_process_t

---@class ObsidianBasesBuffer
---@field bufnr integer
---@field root string
---@field path? string
---@field workspace ObsidianBasesWorkspace
---@field enabled boolean
---@field generation integer
---@field sources ObsidianBasesSource[]
---@field code_action_row? integer

---Find the canonical vault root containing a filesystem path.
---@param path string
---@return string? root
local function vault_root(path)
    local marker = vim.fs.find(".obsidian", { path = path, upward = true, type = "directory" })[1]
    local root = marker and vim.fs.dirname(marker) or nil
    return root and (vim.uv.fs_realpath(root) or root) or nil
end

---Convert an absolute path to a slash-normalised path relative to a vault.
---@param root string
---@param path string
---@return string?
local function relative(root, path)
    local value = vim.fs.relpath(root, vim.uv.fs_realpath(path) or path)
    return value and value:gsub("\\", "/") or nil
end

---Resolve the native worker using the documented override precedence.
---@return string?, string?
local function worker_path()
    local explicit = vim.env.OBSIDIAN_BASE_WORKER
    if explicit and explicit ~= "" then return explicit end
    local configured = config.get().worker_path
    if configured then return configured end
    return installer.resolve_worker()
end

---Close all libuv pipes owned by a worker workspace.
---@param workspace ObsidianBasesWorkspace
local function close_pipes(workspace)
    for _, pipe in ipairs({ workspace.stdin, workspace.stdout, workspace.stderr }) do
        if pipe and not pipe:is_closing() then pipe:close() end
    end
end

---Fail a workspace before closing its transport so no later send can use a stale pipe.
---@param workspace ObsidianBasesWorkspace
---@param error ObsidianBasesProtocolError
local function fail_workspace(workspace, error)
    if workspace.status == "failed" then return end
    workspace.status, workspace.error = "failed", error.message
    if workspace.handle and not workspace.handle:is_closing() then workspace.handle:kill("sigterm") end
    close_pipes(workspace)
    for _, pending in pairs(workspace.pending) do pending.callback(error) end
    workspace.pending = {}
end

---Stop a worker and fail every request that is still awaiting a response.
---@param workspace ObsidianBasesWorkspace
local function stop_workspace(workspace)
    workspace.status = "stopping"
    if workspace.handle and not workspace.handle:is_closing() then workspace.handle:kill("sigterm") end
    close_pipes(workspace)
    for _, pending in pairs(workspace.pending) do
        pending.callback({ code = "worker_stopped", message = "Bases worker stopped" })
    end
    workspace.pending = {}
end

---Encode and send one request to a ready worker, retaining its response callback.
---@param workspace ObsidianBasesWorkspace
---@param method string
---@param params table
---@param callback fun(error: ObsidianBasesProtocolError?, result: table?)
local function send(workspace, method, params, callback)
    if (workspace.status ~= "ready" and method ~= "initialize")
        or not workspace.stdin or workspace.stdin:is_closing()
    then
        callback({ code = "worker_unavailable", message = workspace.error or "worker is not ready" })
        return
    end
    local id = workspace.next_id
    workspace.next_id = id + 1
    workspace.pending[id] = { method = method, callback = callback }
    local ok, encoded = pcall(vim.json.encode, { id = id, request = { method = method, params = params } })
    if not ok then
        workspace.pending[id] = nil
        callback({ code = "encode", message = encoded })
        return
    end
    local wrote, write_error = pcall(workspace.stdin.write, workspace.stdin, encoded .. "\n")
    if not wrote then
        fail_workspace(workspace, { code = "worker_write", message = tostring(write_error) })
    end
end

---Finish callbacks waiting for a workspace to complete its initial handshake.
local function finish_ready_waiters(workspace, error)
    local waiters = workspace.ready_waiters or {}
    workspace.ready_waiters = {}
    for _, waiter in ipairs(waiters) do waiter(error) end
end

---Synchronise one buffer overlay only when its contents actually changed.
---@param workspace ObsidianBasesWorkspace
---@param path string
---@param contents string
---@param callback fun(error: ObsidianBasesProtocolError?, changed: boolean)
local function upsert_overlay(workspace, path, contents, callback)
    if workspace.overlays[path] == contents then callback(nil, false); return end
    workspace.overlays[path] = contents
    send(workspace, "overlay_upsert", { path = path, contents = contents }, function(error)
        if error and workspace.overlays[path] == contents then workspace.overlays[path] = nil end
        callback(error, error == nil)
    end)
end

local function notify_index_changed(workspace, params)
    for _, callback in pairs(index_subscribers[workspace.root] or {}) do
        local ok, err = pcall(callback, vim.deepcopy(params))
        if not ok then vim.schedule(function() vim.notify("Obsidian Bases index subscriber: " .. tostring(err), vim.log.levels.ERROR) end) end
    end
end

---Schedule redraws for every valid buffer attached to a workspace.
---@param workspace ObsidianBasesWorkspace
local function redraw_workspace(workspace)
    for bufnr in pairs(workspace.buffers) do
        if vim.api.nvim_buf_is_valid(bufnr) then vim.api.nvim__redraw({ buf = bufnr, valid = true, flush = false }) end
    end
end

---Dispatch one validated JSON-lines envelope received from the worker.
---@param workspace ObsidianBasesWorkspace
---@param line string
local function on_envelope(workspace, line)
    local ok, envelope = pcall(vim.json.decode, line)
    local valid, error = nil, nil
    if not ok then
        error = { code = "protocol", message = "worker emitted invalid JSON" }
    else
        valid, error = contract.validate_envelope(envelope)
    end
    if error then
        fail_workspace(workspace, error)
        return
    end
    if envelope.event then
        workspace.generation = envelope.event.generation
        workspace.results = {}
        redraw_workspace(workspace)
        for bufnr in pairs(workspace.buffers) do
            local buffer = buffers[bufnr]
            local local_overlay = envelope.event.origin == "overlay" and buffer
                and vim.tbl_contains(envelope.event.paths, buffer.path)
            if not local_overlay then
                local target = bufnr
                vim.schedule(function() M.refresh(target) end)
            end
        end
        notify_index_changed(workspace, envelope.event)
        return
    end
    local pending = workspace.pending[envelope.id]
    if not pending then
        fail_workspace(workspace, { code = "protocol", message = "worker responded to an unknown request" })
        return
    end
    workspace.pending[envelope.id] = nil
    local result, decode_error = contract.decode_response(envelope, pending.method)
    if decode_error and decode_error.code == "protocol" then
        fail_workspace(workspace, decode_error)
        return
    end
    pending.callback(decode_error, result)
end

---Return the vault's worker workspace, creating and initialising it when needed.
---@param root string
---@return ObsidianBasesWorkspace
local function start_workspace(root)
    local workspace = workspaces[root]
    if workspace then return workspace end
    workspace = {
        root = root,
        buffers = {},
        pending = {},
        results = {},
        next_id = 1,
        generation = 0,
        stderr_tail = "",
        crashes = {},
        overlays = {},
        ready_waiters = {},
        status = "starting",
    }
    workspaces[root] = workspace
    local worker, resolution_error = worker_path()
    if not worker then
        workspace.status = "failed"
        workspace.error = resolution_error
            or "native worker is not configured or installed; rebuild obsidian-base.nvim or configure worker_path"
        return workspace
    end
    if vim.fn.filereadable(worker) ~= 1 then
        workspace.status = "failed"
        workspace.error = "worker executable not found: " .. worker
        return workspace
    end
    if vim.fn.executable(worker) ~= 1 then
        workspace.status = "failed"
        workspace.error = "worker file is not executable: " .. worker
        return workspace
    end
    local stdin, stdout, stderr = vim.uv.new_pipe(false), vim.uv.new_pipe(false), vim.uv.new_pipe(false)
    workspace.stdin, workspace.stdout, workspace.stderr = stdin, stdout, stderr
    local handle, spawn_error = vim.uv.spawn(worker, {
        args = {}, stdio = { stdin, stdout, stderr },
    }, function(code, signal)
        vim.schedule(function()
            close_pipes(workspace)
            if workspace.status == "stopping" then return end
            local message = string.format("worker exited (%s/%s)%s", code, signal,
                workspace.stderr_tail ~= "" and ": " .. workspace.stderr_tail or "")
            fail_workspace(workspace, { code = "worker_exit", message = message })
            finish_ready_waiters(workspace, { code = "worker_exit", message = message })
            workspace.handle = nil
            local now = vim.uv.now()
            local crashes = crash_history[root] or {}
            crashes[#crashes + 1] = now
            crashes = vim.tbl_filter(function(time) return now - time <= 30000 end, crashes)
            crash_history[root] = crashes
            if #crashes < 3 then
                workspaces[root] = nil
                for bufnr in pairs(workspace.buffers) do if vim.api.nvim_buf_is_valid(bufnr) then M.attach(bufnr) end end
            else
                workspace.error = "worker crashed repeatedly; run refresh to retry"
            end
        end)
    end)
    if not handle then
        close_pipes(workspace)
        workspace.status = "failed"
        workspace.error = "could not start worker: " .. tostring(spawn_error)
        finish_ready_waiters(workspace, { code = "worker_unavailable", message = workspace.error })
        return workspace
    end
    workspace.handle = handle
    stdout:read_start(function(err, chunk)
        if err then
            fail_workspace(workspace, { code = "worker_read", message = tostring(err) })
            return
        end
        if not chunk then return end
        workspace.stdout_buffer = (workspace.stdout_buffer or "") .. chunk
        while true do
            local newline = workspace.stdout_buffer:find("\n", 1, true)
            if not newline then break end
            local line = workspace.stdout_buffer:sub(1, newline - 1)
            workspace.stdout_buffer = workspace.stdout_buffer:sub(newline + 1)
            if line ~= "" then vim.schedule(function() on_envelope(workspace, line) end) end
        end
    end)
    stderr:read_start(function(_, chunk)
        if chunk then workspace.stderr_tail = (workspace.stderr_tail .. chunk):sub(-8192) end
    end)
    send(workspace, "initialize", { vault_root = root, limits = config.get().limits }, function(error, result)
        if error then
            workspace.status, workspace.error = "failed", error.message
            finish_ready_waiters(workspace, error)
            return
        end
        workspace.status, workspace.generation = "ready", result.generation or 0
        finish_ready_waiters(workspace)
        redraw_workspace(workspace)
        for bufnr in pairs(workspace.buffers) do M.refresh(bufnr) end
    end)
    return workspace
end

---Replace a terminal workspace while preserving every valid buffer attachment.
---@param workspace ObsidianBasesWorkspace
---@return ObsidianBasesWorkspace
local function restart_workspace(workspace)
    local attached = workspace.buffers
    stop_workspace(workspace)
    workspaces[workspace.root] = nil
    crash_history[workspace.root] = {}
    local replacement = start_workspace(workspace.root)
    workspace.buffers = {}
    for bufnr in pairs(attached) do
        local buffer = buffers[bufnr]
        if buffer and buffer.workspace == workspace and vim.api.nvim_buf_is_valid(bufnr) then
            buffer.workspace = replacement
            replacement.buffers[bufnr] = true
        end
    end
    return replacement
end

---Discover `base` fenced blocks and preserve extmarks for unchanged sources.
---@param bufnr integer
---@param root string
---@param path string
---@param previous? ObsidianBasesSource[]
---@return ObsidianBasesSource[], table<string, boolean>
local function fenced_sources(bufnr, root, path, previous)
    local parser = vim.treesitter.get_parser(bufnr, "markdown", { error = false })
    if not parser then return {}, {} end
    base_fence_query = base_fence_query or vim.treesitter.query.parse("markdown", [[
    ((fenced_code_block (info_string (language) @language)) @block)
  ]])
    local sources, namespace, reused = {}, presenter.source_namespace(), {}
    local previous_at_start = {}
    for _, source in ipairs(previous or {}) do
        local mark = vim.api.nvim_buf_get_extmark_by_id(bufnr, namespace, source.start_mark, {})
        if mark[1] ~= nil then previous_at_start[mark[1]] = source end
    end
    for _, tree in ipairs(parser:parse()) do
        for id, node in base_fence_query:iter_captures(tree:root(), bufnr, 0, -1) do
            if base_fence_query.captures[id] == "block" then
                local language = nil
                for child in node:iter_children() do
                    if child:type() == "info_string" then
                        language = vim.treesitter.get_node_text(child, bufnr):match(
                            "^%s*(%S+)")
                    end
                end
                if language == "base" then
                    local start, _, finish = node:range()
                    local source = previous_at_start[start]
                    if source then
                        reused[source.id] = true
                    else
                        source = {
                            id = "inline:" .. relative(root, path) .. ":" .. start,
                            kind = "inline",
                            start_mark = vim.api.nvim_buf_set_extmark(bufnr, namespace, start, 0,
                                { right_gravity = false }),
                            end_mark = vim.api.nvim_buf_set_extmark(bufnr, namespace, finish - 1, 0,
                                { right_gravity = true }),
                        }
                    end
                    source.text = table.concat(vim.api.nvim_buf_get_lines(bufnr, start + 1, finish - 1, false), "\n")
                    source.vault_name, source.loading, source.error, source.result = vim.fs.basename(root), true, nil,
                        nil
                    sources[#sources + 1] = source
                end
            end
        end
    end
    return sources, reused
end

---Discover Base sources in one attached buffer and remove stale source anchors.
---@param buffer ObsidianBasesBuffer
---@return ObsidianBasesSource[]
local function discover(buffer)
    local previous = buffer.sources or {}
    local path = vim.api.nvim_buf_get_name(buffer.bufnr)
    if path:sub(-5) == ".base" then
        local source = previous[1]
        if not source then
            source = {
                id = "file:" .. relative(buffer.root, path),
                kind = "file",
                path = relative(buffer.root, path),
                start_mark = vim.api.nvim_buf_set_extmark(buffer.bufnr, presenter.source_namespace(), 0, 0,
                    { right_gravity = false }),
                end_mark = vim.api.nvim_buf_set_extmark(buffer.bufnr, presenter.source_namespace(),
                    vim.api.nvim_buf_line_count(buffer.bufnr), 0, { right_gravity = true }),
                end_is_eof = true,
            }
        end
        source.text = table.concat(vim.api.nvim_buf_get_lines(buffer.bufnr, 0, -1, false), "\n")
        source.vault_name, source.loading, source.error, source.result = vim.fs.basename(buffer.root), true, nil, nil
        return { source }
    end
    local sources, reused = fenced_sources(buffer.bufnr, buffer.root, path, previous)
    for _, source in ipairs(previous) do
        if not reused[source.id] then
            vim.api.nvim_buf_del_extmark(buffer.bufnr, presenter.source_namespace(), source.start_mark)
            vim.api.nvim_buf_del_extmark(buffer.bufnr, presenter.source_namespace(), source.end_mark)
        end
    end
    return sources
end

---Locate a source by its stable identifier in a buffer's current discovery set.
---@param buffer ObsidianBasesBuffer
---@param id string
---@return ObsidianBasesSource?
local function current_source(buffer, id)
    for _, source in ipairs(buffer.sources) do if source.id == id then return source end end
end

---Read a source's current zero-based extmark range.
---@param buffer ObsidianBasesBuffer
---@param source ObsidianBasesSource
---@return integer?, integer?
local function source_range(buffer, source)
    local start = vim.api.nvim_buf_get_extmark_by_id(
        buffer.bufnr, presenter.source_namespace(), source.start_mark, {}
    )[1]
    local finish = vim.api.nvim_buf_get_extmark_by_id(
        buffer.bufnr, presenter.source_namespace(), source.end_mark, {}
    )[1]
    return start, finish
end

---Return the nearest Markdown heading above a source for chooser labels.
---@param buffer ObsidianBasesBuffer
---@param row integer
---@return string
local function nearest_heading(buffer, row)
    for line = row, 0, -1 do
        local text = vim.api.nvim_buf_get_lines(buffer.bufnr, line, line + 1, false)[1] or ""
        local heading = text:match("^%s*#+%s+(.+)%s*$")
        if heading then return heading end
    end
    return "Base"
end

---Synchronise unsaved text to the worker overlay, then query every discovered source.
---@param buffer ObsidianBasesBuffer
---@param generation integer
local function overlay_then_query(buffer, generation)
    local workspace = buffer.workspace
    local path = buffer.path
    if not path then return end
    upsert_overlay(workspace, path, table.concat(vim.api.nvim_buf_get_lines(buffer.bufnr, 0, -1, false), "\n"),
        function(error)
            if error or not vim.api.nvim_buf_is_valid(buffer.bufnr) or buffer.generation ~= generation then
                if error then for _, source in ipairs(buffer.sources) do source.loading, source.error = false, error end end
                presenter.sync_buffer(buffer.bufnr)
                return
            end
            for _, source in ipairs(buffer.sources) do
                local source_id, text = source.id, source.text
                send(workspace, "query", {
                    source = source.kind == "inline" and { kind = "inline", text = text, source_id = source_id }
                        or { kind = "file", path = source.path, source_id = source_id },
                    host_path = path,
                    preview_rows = config.get().max_preview_rows,
                    view_name = source.selected_view,
                }, function(query_error, result)
                    local current = buffers[buffer.bufnr]
                    local target = current and current_source(current, source_id)
                    if not target or current.generation ~= generation then return end
                    target.loading = false
                    target.error, target.result = query_error, result
                    if result then
                        target.available_views = result.available_views
                        workspace.results[result.result_id] = result
                    elseif query_error and query_error.available_views ~= nil then
                        target.available_views = query_error.available_views
                    end
                    presenter.sync_buffer(buffer.bufnr)
                end)
            end
        end)
end

---Evaluate a file Base embedded in a Markdown host without creating a preview.
---The callback is guarded by the returned cancellation function; cancelling
---does not interrupt the shared worker, but prevents a stale UI commit.
---@param request { bufnr: integer, workspace_root: string, host_path: string, source_path: string, view_name?: string }
---@param callback fun(error: ObsidianBasesProtocolError?, result?: ObsidianBasesTableResult)
---@return fun()
function M.query_embed(request, callback)
    local cancelled = false
    local function cancel() cancelled = true end
    if type(request) ~= "table" or type(request.workspace_root) ~= "string"
        or type(request.host_path) ~= "string" or type(request.source_path) ~= "string"
    then
        callback({ code = "invalid_embed", message = "invalid Base embed request" })
        return cancel
    end
    local root = vim.uv.fs_realpath(request.workspace_root) or request.workspace_root
    local host_path = relative(root, request.host_path)
    if not host_path then
        callback({ code = "invalid_embed", message = "embed host is outside the vault" })
        return cancel
    end
    local workspace = start_workspace(root)
    local function run(ready_error)
        if cancelled then return end
        if ready_error then callback(ready_error); return end
        if workspace.status ~= "ready" then
            callback({ code = "worker_unavailable", message = workspace.error or "Bases worker is not ready" })
            return
        end
        local function query()
            if cancelled then return end
            send(workspace, "query", {
                source = { kind = "file", path = request.source_path, source_id = "embed:" .. request.source_path },
                host_path = host_path,
                preview_rows = config.get().max_preview_rows,
                view_name = request.view_name,
            }, function(error, result)
                if not cancelled then callback(error, result) end
            end)
        end
        if vim.api.nvim_buf_is_valid(request.bufnr) and vim.api.nvim_buf_is_loaded(request.bufnr) then
            upsert_overlay(workspace, host_path,
                table.concat(vim.api.nvim_buf_get_lines(request.bufnr, 0, -1, false), "\n"), function(error, changed)
                    if error then
                        if not cancelled then callback(error) end
                    else
                        local buffer = buffers[request.bufnr]
                        if changed and buffer and buffer.workspace == workspace and buffer.path == host_path then
                            M.refresh(request.bufnr)
                        end
                        query()
                    end
                end)
        else
            query()
        end
    end
    if workspace.status == "ready" then
        run()
    elseif workspace.error then
        run({ code = "worker_unavailable", message = workspace.error })
    else
        workspace.ready_waiters[#workspace.ready_waiters + 1] = run
    end
    return cancel
end

---Subscribe to vault index changes. Used by optional adapters; callers must
---retain and invoke the returned function when their own lifetime ends.
---@param root string
---@param callback fun(event: table)
---@return fun()
function M.subscribe_index(root, callback)
    if type(root) ~= "string" or type(callback) ~= "function" then error("invalid Bases index subscription", 2) end
    root = vim.uv.fs_realpath(root) or root
    local id = next_index_subscriber
    next_index_subscriber = id + 1
    index_subscribers[root] = index_subscribers[root] or {}
    index_subscribers[root][id] = callback
    return function()
        local subscribers = index_subscribers[root]
        if not subscribers then return end
        subscribers[id] = nil
        if not next(subscribers) then index_subscribers[root] = nil end
    end
end

---Clear every source identity and presentation artifact owned by a buffer.
---@param buffer ObsidianBasesBuffer
local function clear_sources(buffer)
    presenter.clear_buffer(buffer.bufnr)
    if vim.api.nvim_buf_is_valid(buffer.bufnr) then
        vim.api.nvim_buf_clear_namespace(buffer.bufnr, presenter.source_namespace(), 0, -1)
    end
    buffer.sources = {}
end

---Release a buffer's workspace membership and its overlay at the bound path.
---@param buffer ObsidianBasesBuffer
local function detach_buffer(buffer)
    local workspace, path = buffer.workspace, buffer.path
    workspace.buffers[buffer.bufnr] = nil
    if path then
        workspace.overlays[path] = nil
        if workspace.status == "ready" then
            send(workspace, "overlay_remove", { path = path }, function()
                workspace.overlays[path] = nil
            end)
        end
    end
    clear_sources(buffer)
end

---Attach an eligible vault buffer to its worker workspace and begin previewing Bases.
---@param bufnr integer
function M.attach(bufnr)
    if not vim.api.nvim_buf_is_valid(bufnr) or not vim.api.nvim_buf_is_loaded(bufnr) then return end
    local path = vim.api.nvim_buf_get_name(bufnr)
    if path == "" then return end
    local eligible = vim.bo[bufnr].filetype == "markdown" or vim.bo[bufnr].filetype == "obsidian_base" or
        path:sub(-5) == ".base"
    local root = eligible and vault_root(path)
    local buffer = buffers[bufnr]
    if not root then
        if buffer then
            detach_buffer(buffer)
            buffers[bufnr] = nil
        end
        return
    end
    local relative_path = relative(root, path)
    if not relative_path then return end
    local workspace = start_workspace(root)
    if buffer and buffer.root == root and buffer.path == relative_path and buffer.workspace == workspace then return end
    if buffer then detach_buffer(buffer) end
    buffer = buffer or { bufnr = bufnr, enabled = true, generation = 0 }
    buffer.root, buffer.path, buffer.workspace = root, relative_path, workspace
    buffers[bufnr], workspace.buffers[bufnr] = buffer, true
    M.refresh(bufnr)
end

---Rediscover Base sources, update the overlay, and request fresh preview results.
---@param bufnr integer
function M.refresh(bufnr)
    local buffer = buffers[bufnr]
    if not buffer then
        M.attach(bufnr)
        return
    end
    if not buffer or not vim.api.nvim_buf_is_valid(bufnr) then return end
    if buffer.workspace.status == "failed" and buffer.workspace.error then
        buffer.workspace = restart_workspace(buffer.workspace)
    end
    buffer.generation = buffer.generation + 1
    buffer.sources = discover(buffer)
    presenter.refresh_foldexpr(bufnr)
    presenter.sync_buffer(bufnr)
    if not buffer.enabled then return end
    if buffer.workspace.status ~= "ready" then
        for _, source in ipairs(buffer.sources) do
            if buffer.workspace.error then
                source.loading, source.error = false, { code = "worker_unavailable", message = buffer.workspace.error }
            end
        end
        presenter.sync_buffer(bufnr)
        return
    end
    overlay_then_query(buffer, buffer.generation)
end

---Toggle preview rendering for an attached buffer.
---@param bufnr integer
function M.toggle(bufnr)
    local buffer = buffers[bufnr]
    if not buffer then
        M.attach(bufnr); buffer = buffers[bufnr]
    end
    if not buffer then return end
    buffer.enabled = not buffer.enabled
    if not buffer.enabled then
        presenter.clear_buffer(bufnr)
        presenter.refresh_foldexpr(bufnr)
    else
        M.refresh(bufnr)
    end
end

---Return a serialisable snapshot of buffer and worker state for diagnostics.
---@param bufnr integer
---@return table?
function M.inspect(bufnr)
    local buffer = buffers[bufnr]
    if not buffer then return nil end
    return {
        root = buffer.root,
        generation = buffer.generation,
        worker_generation = buffer.workspace.generation,
        worker_error = buffer.workspace.error,
        worker_ready = buffer.workspace.status == "ready",
        worker_initializing = buffer.workspace.status == "starting",
        worker_stderr = buffer.workspace.stderr_tail,
        pending_requests = vim.tbl_count(buffer.workspace.pending),
        sources = vim.tbl_map(function(source)
            return { id = source.id, loading = source.loading, error = source.error, result = source.result }
        end, buffer.sources),
    }
end

---Return the stable identifier of the cached source under the cursor.
---@param bufnr integer
---@param row? integer Zero-based cursor row.
---@return string?
function M.source_at_cursor(bufnr, row)
    local buffer = buffers[bufnr]
    if not buffer then return nil end
    if row == nil then
        row = buffer.code_action_row
        if row == nil then
            local wins = vim.fn.win_findbuf(bufnr)
            local win = vim.api.nvim_get_current_win()
            if vim.api.nvim_win_get_buf(win) ~= bufnr then win = wins[1] end
            row = win and vim.api.nvim_win_is_valid(win) and vim.api.nvim_win_get_cursor(win)[1] - 1 or nil
        end
    end
    if row == nil then return nil end
    for _, source in ipairs(buffer.sources) do
        local start, finish = source_range(buffer, source)
        if start and finish and row >= start and row <= finish then return source.id end
    end
end

---Return cached source labels for an interactive chooser.
---@param bufnr integer
---@return table[]
function M.sources(bufnr)
    local buffer = buffers[bufnr]
    if not buffer then return {} end
    local choices = {}
    for _, source in ipairs(buffer.sources) do
        local start = source_range(buffer, source)
        local view = source.selected_view or (source.result and source.result.view and source.result.view.name) or
            "first view"
        choices[#choices + 1] = {
            source_id = source.id,
            label = string.format("%s · line %d · %s", nearest_heading(buffer, start or 0), (start or 0) + 1, view),
        }
    end
    return choices
end

---Return immutable named-view choices for a current source.
---@param bufnr integer
---@param source_id string
---@return table[]
function M.views(bufnr, source_id)
    local buffer = buffers[bufnr]
    local source = buffer and current_source(buffer, source_id)
    if not source then return {} end
    return vim.tbl_map(function(view) return { name = view.name, type = view.type } end,
        source.available_views or {})
end

---Return the current result view name for a source.
---@param bufnr integer
---@param source_id string
---@return string?
function M.result_view(bufnr, source_id)
    local buffer = buffers[bufnr]
    local source = buffer and current_source(buffer, source_id)
    return source and source.result and source.result.view and source.result.view.name or nil
end

---Validate and remember a source's view selection, then rerun its query.
---@param bufnr integer
---@param source_id string
---@param view_name string
---@return boolean
function M.select_view(bufnr, source_id, view_name)
    local buffer = buffers[bufnr]
    local source = buffer and current_source(buffer, source_id)
    if not source then return false end
    local present = vim.iter(source.available_views or {}):any(function(view)
        return view.name == view_name
    end)
    if not present then return false end
    source.selected_view = view_name
    M.refresh(bufnr)
    return true
end

---@param bufnr integer
---@param source_id string
---@param callback fun(error: table?, rows: table[]?)
---Fetch full rows for an already-cached worker result.
function M.fetch_rows(bufnr, source_id, callback)
    local buffer = buffers[bufnr]
    local source = buffer and current_source(buffer, source_id)
    local result = source and source.result
    if not buffer or not source or not result then
        callback({ code = "no_results", message = "Base results are not ready" })
        return
    end
    send(buffer.workspace, "fetch_rows", { result_id = result.result_id }, function(error, value)
        if error then
            callback(error); return
        end
        callback(nil, value.rows or {})
    end)
end

---@param bufnr integer
---@param callback fun(error: table?, worker: table?)
---Request the worker's own diagnostic snapshot.
function M.inspect_worker(bufnr, callback)
    local buffer = buffers[bufnr]
    if not buffer then
        callback({ code = "not_attached", message = "buffer is not attached" }); return
    end
    send(buffer.workspace, "inspect", {}, callback)
end

---Code actions are only exposed for attached Markdown buffers served by obsidian-ls.
---@param bufnr integer
---@return boolean
function M.code_actions_available(bufnr)
    local buffer = buffers[bufnr]
    if not buffer or vim.bo[bufnr].filetype ~= "markdown" or #(buffer.sources or {}) == 0 then return false end
    for _, client in ipairs(vim.lsp.get_clients({ bufnr = bufnr })) do
        if client.name == "obsidian-ls" then return true end
    end
    return false
end

---Remember the buffer/cursor that requested a code action before its deferred
---LSP command runs and focus moves into a picker or code-action window.
---@param bufnr integer
function M.remember_code_action_context(bufnr)
    local buffer = buffers[bufnr]
    if not buffer then return end
    for _, win in ipairs(vim.fn.win_findbuf(bufnr)) do
        if vim.api.nvim_win_get_buf(win) == bufnr then
            buffer.code_action_row = vim.api.nvim_win_get_cursor(win)[1] - 1
            return
        end
    end
end

---Return mutable state for the presenter; this is not part of the public plugin API.
---@param bufnr integer
---@return ObsidianBasesBuffer?
function M.state(bufnr) return buffers[bufnr] end

---Expose fenced-source discovery for focused smoke tests.
---@param bufnr integer
---@param root string
---@return ObsidianBasesSource[], table<string, boolean>
function M._discover(bufnr, root) return fenced_sources(bufnr, root, vim.api.nvim_buf_get_name(bufnr)) end

---Install filetype detection and autocmds that keep Bases previews in sync.
function M.setup()
    if configured then return end
    configured = true
    vim.filetype.add({ extension = { base = "obsidian_base" } })
    presenter.set_state_accessor(M.state)
    vim.api.nvim_create_autocmd({ "BufReadPost", "BufNewFile", "BufEnter" },
        { group = augroup, callback = function(args) M.attach(args.buf) end })
    vim.api.nvim_create_autocmd("BufFilePre", {
        group = augroup,
        callback = function(args)
            local buffer = buffers[args.buf]
            if not buffer then return end
            detach_buffer(buffer)
            buffer.path = nil
        end,
    })
    vim.api.nvim_create_autocmd("BufFilePost", {
        group = augroup,
        callback = function(args)
            local bufnr = args.buf
            local generation = (filepost_generation[bufnr] or 0) + 1
            filepost_generation[bufnr] = generation
            vim.schedule(function()
                if filepost_generation[bufnr] ~= generation then return end
                filepost_generation[bufnr] = nil
                M.attach(bufnr)
            end)
        end,
    })
    -- Filetype detection follows BufReadPost. Attach here as well so Markdown
    -- buffers are not skipped while their filetype is still unset.
    vim.api.nvim_create_autocmd("FileType", {
        group = augroup,
        pattern = { "markdown", "obsidian_base" },
        callback = function(args) M.attach(args.buf) end,
    })
    vim.api.nvim_create_autocmd("BufWritePost", {
        group = augroup,
        callback = function(args)
            local buffer = buffers[args.buf]
            if not buffer or not buffer.path or buffer.workspace.status ~= "ready" then return end
            local workspace, path = buffer.workspace, buffer.path
            workspace.overlays[path] = nil
            send(workspace, "overlay_commit", { path = path }, function(error)
                local current = buffers[args.buf]
                if not error and current and current.workspace == workspace and current.path == path then
                    M.refresh(args.buf)
                end
            end)
        end
    })
    vim.api.nvim_create_autocmd({ "BufUnload", "BufWipeout" }, {
        group = augroup,
        callback = function(args)
            local buffer = buffers[args.buf]
            if not buffer then return end
            detach_buffer(buffer)
            filepost_generation[args.buf] = nil
            buffers[args.buf] = nil
        end
    })
    vim.api.nvim_create_autocmd({ "WinEnter", "BufWinEnter", "VimResized" }, {
        group = augroup,
        callback = function(args)
            if args.buf and buffers[args.buf] then presenter.sync_buffer(args.buf) end
        end
    })
    vim.api.nvim_create_autocmd("VimLeavePre", {
        group = augroup,
        callback = function()
            for _, workspace in pairs(workspaces) do stop_workspace(workspace) end
        end
    })
end

return M
