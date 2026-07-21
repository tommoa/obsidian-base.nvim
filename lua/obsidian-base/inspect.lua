-- Renders coordinator and worker diagnostics into a reusable read-only scratch buffer.
local M = {}

---Append a multi-line inspected value to an existing diagnostic line list.
---@param lines string[]
---@param value unknown
local function append(lines, value)
  for _, line in ipairs(vim.split(vim.inspect(value), "\n", { plain = true })) do lines[#lines + 1] = line end
end

---Render an inspect snapshot into a read-only scratch buffer.
---@param snapshot table
---@param worker? table
---@param bufnr? integer
---@return integer
function M.show(snapshot, worker, bufnr)
  if not bufnr or not vim.api.nvim_buf_is_valid(bufnr) then
    bufnr = vim.fn.bufnr("Obsidian Bases inspect")
    if bufnr > 0 and vim.api.nvim_buf_is_valid(bufnr) then
      vim.api.nvim_set_current_buf(bufnr)
    else
      vim.cmd("botright new")
      bufnr = vim.api.nvim_get_current_buf()
      vim.bo[bufnr].buftype = "nofile"
      vim.bo[bufnr].bufhidden = "wipe"
      vim.bo[bufnr].swapfile = false
      vim.bo[bufnr].filetype = "obsidian-base-inspect"
      vim.api.nvim_buf_set_name(bufnr, "Obsidian Bases inspect")
    end
  end
  local lines = {
    "Obsidian Bases inspect",
    "",
    "Worker",
    "  root: " .. tostring(snapshot.root),
    "  ready: " .. tostring(snapshot.worker_ready),
    "  initializing: " .. tostring(snapshot.worker_initializing),
    "  index generation: " .. tostring(snapshot.worker_generation),
    "  pending requests: " .. tostring(snapshot.pending_requests),
  }
  if snapshot.worker_error then lines[#lines + 1] = "  error: " .. tostring(snapshot.worker_error) end
  if snapshot.worker_stderr and snapshot.worker_stderr ~= "" then
    lines[#lines + 1] = "  stderr: " .. snapshot.worker_stderr
  end
  if worker then
    lines[#lines + 1] = "  worker details:"
    append(lines, worker)
  end
  lines[#lines + 1] = ""
  lines[#lines + 1] = "Sources and cache"
  for _, source in ipairs(snapshot.sources or {}) do
    lines[#lines + 1] = "  " .. source.id
    lines[#lines + 1] = "    loading: " .. tostring(source.loading)
    if source.error then lines[#lines + 1] = "    error: " .. vim.inspect(source.error) end
    if source.result then
      local result = source.result
      lines[#lines + 1] = string.format(
        "    cache: %s · view %s · %d/%d preview rows",
        tostring(result.result_id), result.view and result.view.name or "?",
        result.preview_count or 0, result.view_count or 0
      )
      lines[#lines + 1] = "    timings: " .. vim.inspect(result.timings or {})
      if result.warnings and #result.warnings > 0 then lines[#lines + 1] = "    warnings: " .. vim.inspect(result.warnings) end
    end
  end
  vim.bo[bufnr].modifiable = true
  vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, lines)
  vim.bo[bufnr].modifiable = false
  return bufnr
end

return M
