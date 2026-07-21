-- Installs and resolves checksummed native workers published with tagged releases.
local M = {}

local installing = false
local active_process = nil

local targets = {
    Linux = {
        x86_64 = "x86_64-unknown-linux-musl",
        amd64 = "x86_64-unknown-linux-musl",
        aarch64 = "aarch64-unknown-linux-musl",
        arm64 = "aarch64-unknown-linux-musl",
    },
    Darwin = {
        x86_64 = "x86_64-apple-darwin",
        amd64 = "x86_64-apple-darwin",
        aarch64 = "aarch64-apple-darwin",
        arm64 = "aarch64-apple-darwin",
    },
    Windows_NT = {
        x86_64 = "x86_64-pc-windows-msvc",
        amd64 = "x86_64-pc-windows-msvc",
    },
}

---@param uname? table
---@return string?, string?
local function target_for(uname)
    uname = uname or vim.uv.os_uname()
    local system = targets[uname.sysname]
    local machine = type(uname.machine) == "string" and uname.machine:lower() or ""
    local target = system and system[machine] or nil
    if target then return target end
    return nil, string.format("unsupported worker platform: %s/%s",
        tostring(uname.sysname), tostring(uname.machine))
end

---@return string?, string?
local function plugin_root()
    local cargo = vim.api.nvim_get_runtime_file("worker/Cargo.toml", false)[1]
    if not cargo then return nil, "obsidian-base.nvim is not on runtimepath" end
    local root = vim.fs.normalize(vim.fs.dirname(vim.fs.dirname(cargo)))
    return vim.uv.fs_realpath(root) or vim.fn.fnamemodify(root, ":p")
end

---@param root string
---@return string?, string?
local function release_tag(root)
    if vim.fn.executable("git") ~= 1 then return nil, "required installer tool not found: git" end
    local result = vim.system({ "git", "-C", root, "describe", "--tags", "--exact-match", "HEAD" }, { text = true }):wait()
    local tag = result.code == 0 and vim.trim(result.stdout or "") or ""
    if tag:match("^v%d+%.%d+%.%d+[%w%.%-]*$") then return tag end
    return nil, "prebuilt workers require an exact release tag; build from source or configure worker_path"
end

---@return table?, table?, string?
local function release_for_current_target()
    local root, root_error = plugin_root()
    if not root then return nil, nil, root_error end
    local version, version_error = release_tag(root)
    if not version then return nil, nil, version_error end
    local target, target_error = target_for()
    if not target then return nil, nil, target_error end
    local filename = "obsidian-base-worker-" .. target
        .. (target == "x86_64-pc-windows-msvc" and ".exe" or "")
    return { version = version }, {
        target = target,
        filename = filename,
        url = "https://github.com/tommoa/obsidian-base.nvim/releases/download/" .. version .. "/" .. filename,
    }
end

---@param release table
---@param asset table
---@return string
local function install_path(release, asset)
    return vim.fs.joinpath(vim.fn.stdpath("data"), "obsidian-base.nvim", "workers",
        release.version, asset.target, asset.filename)
end

---@return string
local function executable_name()
    return vim.uv.os_uname().sysname == "Windows_NT" and "obsidian-base-worker.exe"
        or "obsidian-base-worker"
end

---@param root string
---@return string
local function cargo_worker_path(root)
    return vim.fs.joinpath(root, "worker", "target", "release", executable_name())
end

---@param root string
---@return string
local function cargo_pointer_path(root)
    return vim.fs.joinpath(root, "worker", "target", "obsidian-base-worker.json")
end

---@param root string
---@return string?
local function cargo_pointer_worker(root)
    local file = io.open(cargo_pointer_path(root), "rb")
    if not file then return end
    local contents = file:read("*a")
    file:close()
    local ok, value = pcall(vim.json.decode, contents)
    if not ok or type(value) ~= "table" then return end
    if type(value.relative) == "string" then return vim.fs.joinpath(root, value.relative) end
    return type(value.path) == "string" and value.path or nil
end

---@param root string
---@param path string
---@return boolean, string?
local function write_cargo_pointer(root, path)
    local destination = cargo_pointer_path(root)
    local temporary = destination .. ".tmp-" .. tostring(vim.fn.getpid())
    local file, open_error = io.open(temporary, "wb")
    if not file then return false, tostring(open_error) end
    local relative = vim.fs.relpath(root, path)
    local pointer = relative and { relative = relative } or { path = path }
    local written, write_error = file:write(vim.json.encode(pointer))
    file:close()
    if not written then vim.uv.fs_unlink(temporary); return false, tostring(write_error) end
    local renamed, rename_error = vim.uv.fs_rename(temporary, destination)
    if not renamed then vim.uv.fs_unlink(temporary); return false, tostring(rename_error) end
    return true
end

---@param output string
---@return string?
local function cargo_artifact(output)
    local artifact
    for line in (output .. "\n"):gmatch("(.-)\r?\n") do
        local ok, message = pcall(vim.json.decode, line)
        if ok and type(message) == "table" and message.reason == "compiler-artifact"
            and type(message.target) == "table" and message.target.name == "obsidian-base-worker"
            and type(message.executable) == "string" then
            artifact = message.executable
        end
    end
    return artifact
end

---@param output string
---@return string
local function cargo_diagnostics(output)
    local diagnostics = {}
    for line in (output .. "\n"):gmatch("(.-)\r?\n") do
        local ok, message = pcall(vim.json.decode, line)
        local rendered = ok and type(message) == "table" and message.reason == "compiler-message"
            and type(message.message) == "table" and message.message.rendered or nil
        if type(rendered) == "string" and rendered ~= "" then diagnostics[#diagnostics + 1] = vim.trim(rendered) end
    end
    return table.concat(diagnostics, "\n")
end

---@return table<string, string>?
local function cargo_environment()
    if vim.uv.os_uname().sysname ~= "Darwin" or vim.fn.executable("xcrun") ~= 1 then return end
    local clang = vim.system({ "xcrun", "--find", "clang" }, { text = true }):wait()
    local sdk = vim.system({ "xcrun", "--show-sdk-path" }, { text = true }):wait()
    if clang.code ~= 0 or sdk.code ~= 0 then return end
    local target = target_for()
    if not target then return end
    local linker_key = "CARGO_TARGET_" .. target:upper():gsub("%-", "_") .. "_LINKER"
    local library_path = vim.fs.joinpath(vim.trim(sdk.stdout), "usr", "lib")
    if vim.env.LIBRARY_PATH and vim.env.LIBRARY_PATH ~= "" then
        library_path = library_path .. ":" .. vim.env.LIBRARY_PATH
    end
    local environment = {
        SDKROOT = vim.trim(sdk.stdout),
        LIBRARY_PATH = library_path,
    }
    if not vim.env[linker_key] or vim.env[linker_key] == "" then
        environment[linker_key] = vim.trim(clang.stdout)
    end
    return environment
end

---@return string
local function nix_out_link()
    local root = plugin_root()
    local version = root and release_tag(root) or "development"
    version = version or "development"
    return vim.fs.joinpath(vim.fn.stdpath("data"), "obsidian-base.nvim", "workers", "local", "nix", version)
end

---@param path? string
---@return boolean
local function executable(path)
    return type(path) == "string" and path ~= "" and vim.fn.executable(path) == 1
end

---@param path? string
---@return number
local function modified(path)
    local stat = path and vim.uv.fs_stat(path) or nil
    return stat and stat.mtime and (stat.mtime.sec * 1000000000 + stat.mtime.nsec) or 0
end

---Return the manifest-selected managed path without downloading anything.
---@return string?, string?
function M.installed_worker_path()
    local manifest, asset, err = release_for_current_target()
    if not manifest then return nil, err end
    return install_path(manifest, asset)
end

---Resolve an existing managed, locally built, Nix, or PATH worker.
---@return string?, string?
function M.resolve_worker()
    local root, root_error = plugin_root()
    local managed, managed_error = M.installed_worker_path()
    local candidates = { managed }
    if root then
        local pointed, canonical = cargo_pointer_worker(root), cargo_worker_path(root)
        if modified(canonical) > modified(pointed) then
            candidates[#candidates + 1] = canonical
            candidates[#candidates + 1] = pointed
        else
            candidates[#candidates + 1] = pointed
            candidates[#candidates + 1] = canonical
        end
        candidates[#candidates + 1] = vim.fs.joinpath(root, "result", "bin", executable_name())
    end
    candidates[#candidates + 1] = vim.fs.joinpath(nix_out_link(), "bin", executable_name())
    candidates[#candidates + 1] = vim.fn.exepath("obsidian-base-worker")
    for _, candidate in ipairs(candidates) do
        if executable(candidate) then return candidate end
    end
    local reason = root_error or managed_error
    return nil, (reason and reason .. "; " or "")
        .. "worker is not installed; rebuild obsidian-base.nvim or configure worker_path"
end

---@param output string
---@return string?
local function parse_hash(output)
    for line in (output .. "\n"):gmatch("(.-)\r?\n") do
        local first = line:match("^%s*([0-9A-Fa-f]+)%s")
        if first and #first == 64 then return first:lower() end
        local compact = line:gsub("%s", "")
        if #compact == 64 and compact:match("^[0-9A-Fa-f]+$") then return compact:lower() end
    end
end

---@param contents string
---@return string?
local function parse_checksum(contents)
    local hash = contents:match("^%s*([0-9A-Fa-f]+)")
    if hash and #hash == 64 then return hash:lower() end
end

---@param path string
---@return string?, string?
local function read_checksum(path)
    local file, open_error = io.open(path, "rb")
    if not file then return nil, tostring(open_error) end
    local hash = parse_checksum(file:read("*a"))
    file:close()
    if not hash then return nil, "worker checksum file is invalid" end
    return hash
end

---@param sysname string
---@return string, string[]
local function checksum_tool(sysname)
    if sysname == "Darwin" then return "shasum", { "-a", "256" } end
    if sysname == "Windows_NT" then return "certutil", { "-hashfile" } end
    return "sha256sum", {}
end

---@param command string[]
---@param callback fun(result: table)
local function system(command, callback)
    local handle
    handle = vim.system(command, { text = true }, function(result)
        vim.schedule(function()
            if active_process == handle then active_process = nil end
            callback(result)
        end)
    end)
    active_process = handle
    return handle
end

---@param path string
---@param expected_hash string
---@param tool string
---@param tool_args string[]
---@param callback fun(valid: boolean, error?: string)
local function verify(path, expected_hash, tool, tool_args, callback)
    local stat = vim.uv.fs_stat(path)
    if not stat or stat.type ~= "file" then
        callback(false, "worker file is missing")
        return
    end
    local args = vim.list_extend({ tool }, vim.deepcopy(tool_args))
    args[#args + 1] = path
    if tool == "certutil" then args[#args + 1] = "SHA256" end
    system(args, function(result)
        if result.code ~= 0 then
            callback(false, "worker checksum command failed: " .. vim.trim(result.stderr or ""))
            return
        end
        local actual = parse_hash(result.stdout or "")
        if not actual then
            callback(false, "worker checksum command returned an unrecognised result")
        elseif actual ~= expected_hash then
            callback(false, "worker SHA-256 mismatch")
        else
            callback(true)
        end
    end)
end

local function unlink(path)
    if path then vim.uv.fs_unlink(path) end
end

---@param temporary string
---@param destination string
---@return boolean, string?
local function replace(temporary, destination)
    local ok, err = vim.uv.fs_rename(temporary, destination)
    return ok ~= nil, err
end

---@class ObsidianBaseInstallOptions
---@field force? boolean
---@field strategy? "auto"|"download"|"cargo"|"nix"

---Download and atomically install the release asset for the current platform.
---@param opts? ObsidianBaseInstallOptions
---@param callback? fun(error: string?, result?: table)
---@return fun() cancel
local function download(opts, callback)
    opts = opts or {}
    callback = callback or function(error, result)
        if error then
            vim.notify("Obsidian Bases: " .. error, vim.log.levels.ERROR)
        else
            local message = result.skipped and "worker is already installed: " or "installed worker: "
            vim.notify("Obsidian Bases: " .. message .. result.path, vim.log.levels.INFO)
        end
    end
    local finished, cancelled = false, false
    local function cancel()
        if finished then return end
        cancelled, finished = true, true
        if active_process then
            pcall(active_process.kill, active_process, 15)
        else
            installing = false
        end
    end
    local function finish(error, result)
        if finished or cancelled then return end
        finished = true
        installing = false
        callback(error, result)
    end
    if installing then
        callback("a worker installation is already in progress")
        return function() end
    end
    installing = true

    local manifest, asset, release_error = release_for_current_target()
    if not manifest then finish(release_error); return cancel end

    local uname = vim.uv.os_uname()
    local tool, tool_args = checksum_tool(uname.sysname)
    if vim.fn.executable("curl") ~= 1 then finish("required installer tool not found: curl"); return cancel end
    if vim.fn.executable(tool) ~= 1 then finish("required installer tool not found: " .. tool); return cancel end

    local destination = install_path(manifest, asset)
    local checksum_destination = destination .. ".sha256"
    local directory = vim.fs.dirname(destination)
    local mkdir_ok, mkdir_error = pcall(vim.fn.mkdir, directory, "p")
    if not mkdir_ok or vim.fn.isdirectory(directory) ~= 1 then
        finish("could not create worker install directory: " .. tostring(mkdir_error)); return cancel
    end

    local function download()
        if cancelled then return end
        local temporary = destination .. ".download-" .. tostring(vim.fn.getpid()) .. "-" .. tostring(vim.uv.hrtime())
        local checksum_temporary = checksum_destination .. ".download-" .. tostring(vim.fn.getpid()) .. "-" .. tostring(vim.uv.hrtime())
        system({ "curl", "--fail", "--location", "--silent", "--show-error",
            "--proto", "=https", "--proto-redir", "=https", "--tlsv1.2",
            "--connect-timeout", "30", "--max-time", "110",
            "--output", temporary, asset.url }, function(result)
            if cancelled then unlink(temporary); unlink(checksum_temporary); installing = false; return end
            if result.code ~= 0 then
                unlink(temporary)
                finish("worker download failed: " .. vim.trim(result.stderr or ""))
                return
            end
            system({ "curl", "--fail", "--location", "--silent", "--show-error",
                "--proto", "=https", "--proto-redir", "=https", "--tlsv1.2",
                "--connect-timeout", "30", "--max-time", "110",
                "--output", checksum_temporary, asset.url .. ".sha256" }, function(checksum_result)
                if cancelled then unlink(temporary); unlink(checksum_temporary); installing = false; return end
                if checksum_result.code ~= 0 then
                    unlink(temporary); unlink(checksum_temporary)
                    finish("worker checksum download failed: " .. vim.trim(checksum_result.stderr or ""))
                    return
                end
                local expected_hash, checksum_error = read_checksum(checksum_temporary)
                if not expected_hash then
                    unlink(temporary); unlink(checksum_temporary)
                    finish(checksum_error)
                    return
                end
                verify(temporary, expected_hash, tool, tool_args, function(valid, verify_error)
                    if cancelled then unlink(temporary); unlink(checksum_temporary); installing = false; return end
                    if not valid then
                        unlink(temporary); unlink(checksum_temporary)
                        finish(verify_error)
                        return
                    end
                    if uname.sysname ~= "Windows_NT" then
                        local chmod_ok, chmod_error = vim.uv.fs_chmod(temporary, 493)
                        if not chmod_ok then
                            unlink(temporary); unlink(checksum_temporary)
                            finish("could not make worker executable: " .. tostring(chmod_error))
                            return
                        end
                    end
                    local checksum_replaced, checksum_replace_error = replace(checksum_temporary, checksum_destination)
                    if not checksum_replaced then
                        unlink(temporary); unlink(checksum_temporary)
                        finish("could not replace worker checksum: " .. tostring(checksum_replace_error))
                        return
                    end
                    local replaced, replace_error = replace(temporary, destination)
                    if not replaced then
                        unlink(temporary)
                        local hint = uname.sysname == "Windows_NT"
                            and "; close worker-backed buffers and retry before the worker starts" or ""
                        finish("could not replace worker executable: " .. tostring(replace_error) .. hint)
                        return
                    end
                    finish(nil, { path = destination, skipped = false })
                end)
            end)
        end)
    end

    if opts.force then download(); return cancel end
    local expected_hash = read_checksum(checksum_destination)
    if not expected_hash then download(); return cancel end
    verify(destination, expected_hash, tool, tool_args, function(valid)
        if cancelled then installing = false; return end
        if valid then finish(nil, { path = destination, skipped = true }) else download() end
    end)
    return cancel
end

---@param strategy "cargo"|"nix"
---@param callback fun(error: string?, result?: table)
---@return fun() cancel
local function build(strategy, callback)
    local finished, cancelled = false, false
    local function cancel()
        if finished then return end
        cancelled, finished = true, true
        if not active_process then installing = false end
    end
    local function finish(error, result)
        if finished or cancelled then return end
        finished, installing = true, false
        callback(error, result)
    end
    if installing then
        callback("a worker installation is already in progress")
        return function() end
    end
    installing = true

    local root, root_error = plugin_root()
    if not root then finish(root_error); return cancel end
    if vim.fn.executable(strategy) ~= 1 then
        finish("required build tool not found: " .. strategy)
        return cancel
    end

    local command, destination
    if strategy == "cargo" then
        destination = cargo_worker_path(root)
        command = {
            "cargo", "build", "--release", "--locked",
            "--message-format", "json-render-diagnostics",
            "--manifest-path", vim.fs.joinpath(root, "worker", "Cargo.toml"),
            "--target-dir", vim.fs.joinpath(root, "worker", "target"),
        }
    else
        local out_link = nix_out_link()
        local ok, mkdir_error = pcall(vim.fn.mkdir, vim.fs.dirname(out_link), "p")
        if not ok then
            finish("could not create Nix worker directory: " .. tostring(mkdir_error))
            return cancel
        end
        destination = vim.fs.joinpath(out_link, "bin", executable_name())
        command = { "nix", "build", ".#worker", "--out-link", out_link }
    end

    local retried_macos_linker = false
    local function run(environment)
        local process_opts = { cwd = root, text = true, env = environment }
        local handle
        handle = vim.system(command, process_opts, function(result)
            vim.schedule(function()
                if active_process == handle then active_process = nil end
                if cancelled then installing = false; return end
                if result.code ~= 0 then
                    local detail = strategy == "cargo" and cargo_diagnostics(result.stdout or "") or ""
                    local stderr = vim.trim(result.stderr or "")
                    if stderr ~= "" then detail = detail ~= "" and (detail .. "\n" .. stderr) or stderr end
                    if detail == "" then detail = vim.trim(result.stdout or "") end
                    local fallback = strategy == "cargo" and not retried_macos_linker
                        and detail:find("library not found for %-liconv") and cargo_environment() or nil
                    if fallback then
                        retried_macos_linker = true
                        run(fallback)
                        return
                    end
                    finish(string.format("%s build failed (%d): %s", strategy, result.code, detail))
                    return
                end
                if strategy == "cargo" then destination = cargo_artifact(result.stdout or "") or destination end
                if not executable(destination) then
                    finish(strategy .. " build completed without producing an executable worker: " .. destination)
                    return
                end
                if strategy == "cargo" then
                    local pointer_ok, pointer_error = write_cargo_pointer(root, destination)
                    if not pointer_ok then
                        finish("could not record Cargo worker path: " .. tostring(pointer_error))
                        return
                    end
                end
                finish(nil, { path = destination, skipped = false, strategy = strategy })
            end)
        end)
        active_process = handle
    end
    run(nil)
    return cancel
end

---@return ("cargo"|"nix")[]
local function available_builders()
    local builders = {}
    if vim.fn.executable("cargo") == 1 then builders[#builders + 1] = "cargo" end
    if vim.fn.executable("nix") == 1 then builders[#builders + 1] = "nix" end
    return builders
end

---@param opts ObsidianBaseInstallOptions
---@param callback fun(error: string?, result?: table)
---@return fun() cancel
local function auto(opts, callback)
    local cancelled, current_cancel = false, nil
    local function cancel()
        cancelled = true
        if current_cancel then current_cancel() end
    end
    local function fallback(download_error)
        if cancelled then return end
        local builders = available_builders()
        if #builders == 0 then
            callback(download_error .. "; install Cargo or Nix, or configure worker_path")
            return
        end
        local errors, at = { download_error }, 0
        local function try_next()
            if cancelled then return end
            at = at + 1
            local strategy = builders[at]
            if not strategy then
                callback(table.concat(errors, "; "))
                return
            end
            current_cancel = build(strategy, function(build_error, result)
                if cancelled then return end
                if build_error then
                    errors[#errors + 1] = strategy .. " fallback failed: " .. build_error
                    try_next()
                else
                    callback(nil, result)
                end
            end)
        end
        try_next()
    end

    local manifest, asset, release_error = release_for_current_target()
    if manifest and asset then
        local fell_back = false
        local download_cancel = download(opts, function(download_error, result)
            if cancelled then return end
            if download_error then
                fell_back = true
                fallback(download_error)
            else
                callback(nil, result)
            end
        end)
        if not fell_back then current_cancel = download_cancel end
    else fallback(release_error or "no prebuilt worker is available") end
    return cancel
end

---Install or build a worker with an explicit or automatic strategy.
---@param opts? ObsidianBaseInstallOptions
---@param callback? fun(error: string?, result?: table)
---@return fun() cancel
function M.install(opts, callback)
    if opts == nil then opts = {} end
    if type(opts) ~= "table" then error("obsidian-base install options must be a table", 2) end
    if callback ~= nil and type(callback) ~= "function" then
        error("obsidian-base install callback must be a function", 2)
    end
    callback = callback or function(error, result)
        if error then
            vim.notify("Obsidian Bases: " .. error, vim.log.levels.ERROR)
        else
            local action = result.skipped and "worker is already installed: " or "worker is ready: "
            vim.notify("Obsidian Bases: " .. action .. result.path, vim.log.levels.INFO)
        end
    end
    local strategy = opts.strategy or "auto"
    if strategy == "download" then return download(opts, callback) end
    if strategy == "cargo" or strategy == "nix" then return build(strategy, callback) end
    if strategy == "auto" then return auto(opts, callback) end
    callback("unknown worker installation strategy: " .. tostring(strategy))
    return function() end
end

M._parse_hash = parse_hash
M._parse_checksum = parse_checksum
M._target_for = target_for
M._is_installing = function() return installing end

return M
