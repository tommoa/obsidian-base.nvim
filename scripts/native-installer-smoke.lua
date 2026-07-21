-- Focused native-launch and release-installer smoke coverage with local fake tools.
local script = debug.getinfo(1, "S").source:sub(2)
local root = vim.fs.dirname(vim.fs.dirname(script))
vim.opt.runtimepath:prepend(root)

local function assert_match(value, pattern, message)
    assert(type(value) == "string" and value:match(pattern), message .. ": " .. tostring(value))
end

local function write(path, contents)
    assert(vim.fn.mkdir(vim.fs.dirname(path), "p") == 1 or vim.fn.isdirectory(vim.fs.dirname(path)) == 1)
    local file = assert(io.open(path, "wb"))
    assert(file:write(contents))
    file:close()
end

local function read(path)
    local file = assert(io.open(path, "rb"))
    local contents = assert(file:read("*a"))
    file:close()
    return contents
end

local installer = require("obsidian-base.installer")

local function run_install(opts)
    opts.strategy = opts.strategy or "download"
    local done, install_error, result = false, nil, nil
    installer.install(opts, function(err, value)
        install_error, result, done = err, value, true
    end)
    assert(vim.wait(5000, function() return done end, 10), "installer callback timed out")
    return install_error, result
end

local function assert_no_prebuilt(value, message)
    assert(type(value) == "string" and (value:match("exact release tag")
            or value:match("required installer tool not found: git")),
        message .. ": " .. tostring(value))
end

local untagged_error = run_install({})
assert_no_prebuilt(untagged_error, "untagged release error was not actionable")
local public_done, public_error = false, nil
require("obsidian-base").install_worker({ strategy = "download" }, function(err)
    public_error, public_done = err, true
end)
assert(public_done, "public installer API did not preserve immediate completion")
assert_no_prebuilt(public_error, "public installer API did not propagate failure")

local temporary = vim.fn.tempname() .. " installer smoke"
local runtime = vim.fs.joinpath(temporary, "runtime")
local tools = vim.fs.joinpath(temporary, "fake tools")
local empty_tools = vim.fs.joinpath(temporary, "empty tools")
local source = vim.fs.joinpath(temporary, "download source")
assert(vim.fn.mkdir(vim.fs.joinpath(runtime, "worker"), "p") == 1)
assert(vim.fn.mkdir(tools, "p") == 1)
assert(vim.fn.mkdir(empty_tools, "p") == 1)
write(vim.fs.joinpath(runtime, "worker", "Cargo.toml"), "[package]\nname='installer-smoke'\nversion='0.0.0'\n")

local native_worker = [[#!/bin/sh
printf '%s' "$#" > "$OBSIDIAN_BASE_TEST_MARKER"
IFS= read -r request || exit 0
printf '{"id":1,"response":{"type":"success","result":{"method":"initialize","data":{"generation":1,"files":1}}}}\n'
while IFS= read -r request; do :; done
]]
write(source, native_worker)

local curl = [[#!/bin/sh
if [ "${FAKE_CURL_FAIL:-}" = "1" ]; then
  printf 'simulated interrupted download\n' >&2
  exit 22
fi
if [ -n "${FAKE_CURL_SLEEP:-}" ]; then
  sleep "$FAKE_CURL_SLEEP"
fi
output=
url=
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--output" ]; then
    shift
    output=$1
  else
    url=$1
  fi
  shift
done
case "$url" in
  *.sha256*) source=$FAKE_CHECKSUM_SOURCE ;;
  *) source=$FAKE_CURL_SOURCE ;;
esac
exec /bin/cp "$source" "$output"
]]
local shasum = [[#!/bin/sh
printf '%s  %s\n' "$FAKE_CHECKSUM" "${3:-${1}}"
]]
local sha256sum = [[#!/bin/sh
printf '%s  %s\n' "$FAKE_CHECKSUM" "$1"
]]
local certutil = [[#!/bin/sh
printf 'SHA256 hash of file:\n%s\nCertUtil: completed successfully\n' "$FAKE_CHECKSUM"
]]
local cargo = [[#!/bin/sh
if [ "${FAKE_CARGO_FAIL:-}" = "1" ]; then
  printf '{"reason":"compiler-message","message":{"rendered":"simulated rendered Cargo error"}}\n'
  printf 'simulated Cargo failure\n' >&2
  exit 1
fi
target=
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--target-dir" ]; then shift; target=$1; fi
  shift
done
artifact="$target/fake-target/release/$FAKE_WORKER_NAME"
mkdir -p "$(dirname "$artifact")"
cp "$FAKE_CURL_SOURCE" "$artifact"
chmod +x "$artifact"
printf '{"reason":"compiler-artifact","target":{"name":"obsidian-base-worker"},"executable":"%s"}\n' "$artifact"
]]
local nix = [[#!/bin/sh
out=
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--out-link" ]; then shift; out=$1; fi
  shift
done
mkdir -p "$out/bin"
cp "$FAKE_CURL_SOURCE" "$out/bin/$FAKE_WORKER_NAME"
chmod +x "$out/bin/$FAKE_WORKER_NAME"
]]
local git = [[#!/bin/sh
printf '%s\n' "$FAKE_RELEASE_TAG"
]]
for name, contents in pairs({
    curl = curl,
    shasum = shasum,
    sha256sum = sha256sum,
    certutil = certutil,
    cargo = cargo,
    git = git,
    nix = nix,
}) do
    local path = vim.fs.joinpath(tools, name)
    write(path, contents)
    assert(vim.uv.fs_chmod(path, 493))
end
write(vim.fs.joinpath(empty_tools, "git"), git)
assert(vim.uv.fs_chmod(vim.fs.joinpath(empty_tools, "git"), 493))

local target = assert(installer._target_for(vim.uv.os_uname()))
local filename = "obsidian-base-worker-" .. target .. (target:match("windows") and ".exe" or "")
vim.env.FAKE_WORKER_NAME = target:match("windows") and "obsidian-base-worker.exe" or "obsidian-base-worker"
local test_version = "v9.9." .. vim.fn.getpid() .. "-test"
local checksum_source = vim.fs.joinpath(temporary, "checksum source")
local function write_checksum(contents, hash)
    vim.env.FAKE_CHECKSUM = vim.fn.sha256(contents)
    write(checksum_source, (hash or vim.fn.sha256(contents)) .. "  " .. filename .. "\n")
end
write_checksum(native_worker)
vim.env.FAKE_RELEASE_TAG = test_version
vim.opt.runtimepath:prepend(runtime)

local old_path = vim.env.PATH
vim.env.FAKE_CURL_SOURCE = source
vim.env.FAKE_CHECKSUM_SOURCE = checksum_source
vim.env.PATH = empty_tools
local missing_error = run_install({})
assert_match(missing_error, "required installer tool not found", "missing tools were not rejected")
local auto_missing_error = run_install({ strategy = "auto" })
assert_match(auto_missing_error, "install Cargo or Nix", "auto strategy did not report missing fallbacks")
vim.env.PATH = tools .. ":" .. old_path

local cargo_error, cargo_result = run_install({ strategy = "cargo" })
assert(not cargo_error and cargo_result.strategy == "cargo", cargo_error)
assert(installer.resolve_worker() == cargo_result.path, "resolver did not select the local Cargo worker")
local canonical_cargo = vim.fs.joinpath(runtime, "worker", "target", "release", vim.env.FAKE_WORKER_NAME)
write(canonical_cargo, native_worker)
assert(vim.uv.fs_chmod(canonical_cargo, 493))
local pointed_stat = assert(vim.uv.fs_stat(cargo_result.path))
assert(vim.uv.fs_utime(canonical_cargo, pointed_stat.mtime.sec + 1, pointed_stat.mtime.sec + 1))
local canonical_real = assert(vim.uv.fs_realpath(canonical_cargo))
assert(installer.resolve_worker() == canonical_real,
    "newer canonical Cargo build did not supersede its pointer: " .. tostring(installer.resolve_worker()))
assert(vim.uv.fs_unlink(canonical_cargo), "could not remove canonical fake Cargo worker")
assert(vim.uv.fs_unlink(cargo_result.path), "could not remove fake Cargo worker")

local nix_error, nix_result = run_install({ strategy = "nix" })
assert(not nix_error and nix_result.strategy == "nix", nix_error)
assert(installer.resolve_worker() == nix_result.path, "resolver did not select the Nix worker")

vim.env.FAKE_CURL_FAIL = "1"
local auto_error, auto_result = run_install({ strategy = "auto" })
vim.env.FAKE_CURL_FAIL = nil
assert(not auto_error and auto_result.strategy == "cargo", auto_error)
assert(installer.resolve_worker() == auto_result.path, "auto fallback did not produce a Cargo worker")

vim.env.FAKE_CURL_FAIL = "1"
vim.env.FAKE_CARGO_FAIL = "1"
local cargo_failure = run_install({ strategy = "cargo" })
assert_match(cargo_failure, "simulated rendered Cargo error", "Cargo rendered diagnostic was discarded")
local nix_fallback_error, nix_fallback_result = run_install({ strategy = "auto" })
vim.env.FAKE_CURL_FAIL = nil
vim.env.FAKE_CARGO_FAIL = nil
assert(not nix_fallback_error and nix_fallback_result.strategy == "nix", nix_fallback_error)

local install_error, installed = run_install({})
assert(not install_error, install_error)
assert(installed and not installed.skipped, "initial worker installation did not run")
assert(vim.fn.readfile(installed.path, "b")[1]:match("^#!/bin/sh"), "installed worker contents changed")
if vim.uv.os_uname().sysname ~= "Windows_NT" then
    assert(vim.fn.executable(installed.path) == 1, "installed worker is not executable")
end

local skip_error, skipped = run_install({})
assert(not skip_error and skipped.skipped, "verified existing worker was not skipped")

vim.env.FAKE_CURL_SLEEP = "1"
local first_done, first_error = false, nil
installer.install({ force = true, strategy = "download" }, function(err)
    first_error, first_done = err, true
end)
local rejected_error
local rejected_cancel = installer.install({ strategy = "download" }, function(err) rejected_error = err end)
assert_match(rejected_error, "already in progress", "concurrent installer was not rejected")
rejected_cancel()
assert(vim.wait(3000, function() return first_done end, 10), "active installer did not finish")
vim.env.FAKE_CURL_SLEEP = nil
assert(not first_error, first_error)
assert(read(installed.path) == native_worker, "rejected install cancelled the active installer")

vim.env.FAKE_CURL_SLEEP = "2"
local cancelled = installer.install({ force = true, strategy = "download" }, function()
    error("cancelled installer invoked its completion callback")
end)
cancelled()
vim.env.FAKE_CURL_SLEEP = nil
assert(read(installed.path) == native_worker, "cancelled install replaced the prior worker")
assert(vim.wait(3000, function() return not installer._is_installing() end, 10),
    "cancelled installer did not release its lock after exiting")

vim.env.FAKE_CURL_FAIL = "1"
local interrupted_error = run_install({ force = true })
vim.env.FAKE_CURL_FAIL = nil
assert_match(interrupted_error, "download failed", "interrupted download did not fail")
assert(read(installed.path) == native_worker, "failed forced download replaced the prior worker")

write_checksum(native_worker, string.rep("0", 64))
local hash_error = run_install({ force = true })
assert_match(hash_error, "SHA%-256 mismatch", "hash mismatch was not rejected")
assert(vim.fn.executable(installed.path) == 1, "hash failure removed the prior worker")

local binary = "abc\0def\n"
write(source, binary)
write_checksum(binary)
local binary_error, binary_result = run_install({ force = true })
assert(not binary_error and vim.uv.fs_stat(binary_result.path).size == #binary,
    "binary payload containing NUL was not installed exactly")

write(source, native_worker)
write_checksum(native_worker)
local restore_error, restored = run_install({ force = true })
assert(not restore_error and restored.path == installed.path, restore_error)

local expected_hash = string.rep("a", 64)
assert(installer._parse_hash(expected_hash .. "  worker.exe\n") == expected_hash, "sha256sum parsing failed")
assert(installer._parse_hash("SHA256 hash of file:\r\n" .. expected_hash:upper() .. "\r\nCertUtil: ok\r\n")
    == expected_hash, "certutil parsing failed")
assert(installer._parse_checksum(expected_hash .. "  worker.exe\n") == expected_hash,
    "release checksum parsing failed")
assert(not installer._target_for({ sysname = "Plan9", machine = "mips" }), "unsupported target was accepted")

assert(vim.fn.exists(":ObsidianBaseInstall") == 0, "removed installer command was registered")
assert(not pcall(require("obsidian-base.config").setup, { node = "node" }), "removed node option was accepted")

local config = require("obsidian-base.config")
local coordinator = require("obsidian-base.coordinator")
vim.env.PATH = tools
assert(vim.fn.executable("node") == 0, "native launch test unexpectedly has Node on PATH")
local build = assert(loadfile(vim.fs.joinpath(root, "build.lua")))
local build_ok, build_error = pcall(build)
assert(build_ok, "lazy build entrypoint failed: " .. tostring(build_error))
local function launch(label, configured, environment)
    local vault = vim.fs.joinpath(temporary, "vault " .. label)
    local note = vim.fs.joinpath(vault, "note.md")
    local marker = vim.fs.joinpath(temporary, "marker " .. label)
    assert(vim.fn.mkdir(vim.fs.joinpath(vault, ".obsidian"), "p") == 1)
    write(note, "plain markdown\n")
    config.setup({ worker_path = configured })
    vim.env.OBSIDIAN_BASE_WORKER = environment
    vim.env.OBSIDIAN_BASE_TEST_MARKER = marker
    vim.cmd.edit(vim.fn.fnameescape(note))
    local bufnr = vim.api.nvim_get_current_buf()
    vim.bo[bufnr].filetype = "markdown"
    coordinator.attach(bufnr)
    assert(vim.wait(3000, function()
        local state = coordinator.inspect(bufnr)
        return state and state.worker_ready
    end, 10), "native worker did not become ready for " .. label)
    assert(vim.fn.readfile(marker)[1] == "0", "native worker received launcher arguments for " .. label)
end

launch("environment", "/worker/path/that/must/not/run", restored.path)
launch("configuration", restored.path, nil)
launch("managed", nil, nil)

vim.env.PATH = old_path
vim.env.FAKE_CURL_SOURCE = nil
vim.env.FAKE_CHECKSUM_SOURCE = nil
vim.env.FAKE_CHECKSUM = nil
vim.env.FAKE_WORKER_NAME = nil
vim.env.FAKE_RELEASE_TAG = nil
vim.env.OBSIDIAN_BASE_WORKER = nil
vim.env.OBSIDIAN_BASE_TEST_MARKER = nil
vim.fn.delete(vim.fs.dirname(vim.fs.dirname(restored.path)), "rf")
vim.fn.delete(vim.fs.dirname(vim.fs.dirname(nix_result.path)), "rf")
vim.fn.delete(temporary, "rf")
print("obsidian-base native installer smoke passed")
