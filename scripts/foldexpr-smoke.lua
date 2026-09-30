-- Deterministic fold lookup cost and moving-anchor coverage without a worker.
vim.opt.runtimepath:append(vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":h:h"))
local presenter = require("obsidian-base.presenter")
local namespace = presenter.source_namespace()
local states = {}
presenter.set_state_accessor(function(bufnr) return states[bufnr] end)

local function fixture(lines)
  local bufnr = vim.api.nvim_create_buf(true, false)
  vim.api.nvim_set_current_buf(bufnr)
  vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, lines)
  local state = { enabled = true, sources = {} }
  states[bufnr] = state
  return bufnr, state
end

local function source(bufnr, start, finish, eof)
  return {
    start_mark = vim.api.nvim_buf_set_extmark(bufnr, namespace, start, 0, { right_gravity = false }),
    end_mark = vim.api.nvim_buf_set_extmark(bufnr, namespace, finish, 0, { right_gravity = true }),
    end_is_eof = eof,
  }
end

local get_mark, iterate = vim.api.nvim_buf_get_extmark_by_id, ipairs
local reads, visits = 0, 0
vim.api.nvim_buf_get_extmark_by_id = function(bufnr, ns, ...)
  if ns == namespace then reads = reads + 1 end
  return get_mark(bufnr, ns, ...)
end
-- Count source enumeration separately from extmark reads.
_G.ipairs = function(values)
  local iterator, table_value, index = iterate(values)
  local state = states[vim.api.nvim_get_current_buf()]
  if not state or values ~= state.sources then return iterator, table_value, index end
  return function(t, i)
    local next_index, value = iterator(t, i)
    if next_index then visits = visits + 1 end
    return next_index, value
  end, table_value, index
end

for _, count in iterate({ 1, 10, 100 }) do
  local bufnr, state = fixture(vim.fn["repeat"]({ "plain" }, 10000))
  for i = 1, count do state.sources[i] = source(bufnr, (i - 1) * 90, (i - 1) * 90 + 2) end
  reads, visits = 0, 0
  for line = 1, 10000 do
    local offset = line - 1
    local expected = 0
    if offset < count * 90 then
      local within = offset % 90
      if within == 0 then expected = ">1" elseif within == 1 then expected = 1 elseif within == 2 then expected = "<1" end
    end
    assert(presenter.foldexpr(line) == expected, "incorrect fold at " .. line)
  end
  assert(reads == count * 2 and visits == count,
    "fold pass must read and enumerate each source only once: " .. reads .. "/" .. visits)
  for line = 10000, 1, -1 do presenter.foldexpr(line) end
  assert(reads == count * 2 and visits == count, "unchanged lookups must reuse ranges in any order")
  vim.api.nvim_buf_set_lines(bufnr, 9999, 10000, false, { "edited" })
  presenter.foldexpr(10000)
  assert(reads == count * 4 and visits == count * 2, "edit must rebuild once")
  presenter.foldexpr(1)
  assert(reads == count * 4, "edit cache was not reused")
  vim.api.nvim_buf_delete(bufnr, { force = true })
  states[bufnr] = nil
end
_G.ipairs = iterate
vim.api.nvim_buf_get_extmark_by_id = get_mark

-- The old linear lookup is the behavioral oracle, including overlaps after
-- edits, collapsed/missing anchors, and EOF's exclusive end marker.
local function linear(bufnr, state, line, level)
  for _, item in ipairs(state.sources) do
    local start = get_mark(bufnr, namespace, item.start_mark, {})[1]
    local finish = get_mark(bufnr, namespace, item.end_mark, {})[1]
    if start and finish then
      if item.end_is_eof then finish = finish - 1 end
      if finish > start then
        if line == start + 1 then return ">" .. level end
        if line == finish + 1 then return "<" .. level end
        if line > start + 1 and line < finish + 1 then return level end
      end
    end
  end
  return 2
end

local function compare(bufnr, state)
  -- Visit out of order: foldexpr callers need not request a sequential pass.
  for line = vim.api.nvim_buf_line_count(bufnr), 1, -1 do
    assert(presenter.foldexpr(line, function() return ">2" end) == linear(bufnr, state, line, 3),
      "moving fold mismatch at " .. line)
  end
end

local bufnr, state = fixture({ "before", "```base", "one", "```", "gap", "```base", "two", "```", "after" })
state.sources = { source(bufnr, 1, 3), source(bufnr, 5, 7) }
compare(bufnr, state)
-- Separate the API edit from fixture creation in Neovim's undo history.
vim.cmd("let &undolevels = &undolevels")
local before = vim.api.nvim_buf_get_lines(bufnr, 0, -1, false)
vim.api.nvim_buf_set_lines(bufnr, 0, 0, false, { "inserted above" })
compare(bufnr, state)
vim.cmd.undo()
assert(vim.deep_equal(vim.api.nvim_buf_get_lines(bufnr, 0, -1, false), before), "undo fixture failed")
compare(bufnr, state)
vim.cmd.redo()
compare(bufnr, state)
vim.api.nvim_buf_set_lines(bufnr, 3, 3, false, { "inside" })
compare(bufnr, state)
vim.api.nvim_buf_set_lines(bufnr, 6, 6, false, { "between" })
compare(bufnr, state)
-- Replacement can make formerly separate ranges overlap. First-source
-- precedence must survive, including a collapsed range being skipped.
vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, { "replacement", "body", "tail" })
compare(bufnr, state)

-- Discovery replaces its source array without changing buffer text.
local discovery_tick = vim.api.nvim_buf_get_changedtick(bufnr)
state.sources = { source(bufnr, 0, 0), source(bufnr, 0, 1), source(bufnr, 0, 2) }
compare(bufnr, state)
assert(presenter.foldexpr(2) == "<1", "overlapping sources lost first-match precedence")
state.sources = { source(bufnr, 0, 2) }
compare(bufnr, state)
assert(vim.api.nvim_buf_get_changedtick(bufnr) == discovery_tick, "source replacement fixture changed text")
local tick = vim.api.nvim_buf_get_changedtick(bufnr)
vim.api.nvim_buf_set_extmark(bufnr, namespace, 1, 0, { id = state.sources[1].start_mark, right_gravity = false })
presenter.refresh_foldexpr(bufnr)
assert(vim.api.nvim_buf_get_changedtick(bufnr) == tick, "extmark-only refresh fixture changed text")
compare(bufnr, state)
state.enabled = false
assert(presenter.foldexpr(2, function() return "<4" end) == 4, "disabled state must preserve fallback")
state.enabled = true
assert(presenter.foldexpr(2, function() error("fallback failure") end) == ">1", "fallback errors must become zero")
assert(presenter.foldexpr(2, function() error("unused override fallback") end, 7) == ">7", "absolute level was cached")
vim.api.nvim_buf_del_extmark(bufnr, namespace, state.sources[1].end_mark)
presenter.refresh_foldexpr(bufnr)
assert(presenter.foldexpr(2) == 0, "missing anchors must not fold")
state.sources = { source(bufnr, 0, 2) }
assert(presenter.foldexpr(1) == ">1", "clear fixture did not prime a fold")
local clear_tick = vim.api.nvim_buf_get_changedtick(bufnr)
-- Match coordinator.clear_sources: clearing anchors and replacing sources does
-- not call refresh_foldexpr during BufFilePre's detach/rename gap.
presenter.clear_buffer(bufnr)
vim.api.nvim_buf_clear_namespace(bufnr, namespace, 0, -1)
state.sources = {}
assert(vim.api.nvim_buf_get_changedtick(bufnr) == clear_tick, "source clear fixture changed text")
assert(presenter.foldexpr(2, function() return 5 end) == 5, "source removal must preserve fallback")

local eof_bufnr, eof_state = fixture({ "first", "middle", "last" })
eof_state.sources = { source(eof_bufnr, 0, 3, true) }
compare(eof_bufnr, eof_state)
assert(presenter.foldexpr(3) == "<1", "EOF end must fold the final real line")
vim.api.nvim_buf_set_lines(eof_bufnr, -1, -1, false, { "appended" })
compare(eof_bufnr, eof_state)
assert(presenter.foldexpr(4) == "<1", "EOF anchor must move on append")
vim.api.nvim_buf_set_lines(eof_bufnr, 0, -1, false, { "only" })
compare(eof_bufnr, eof_state)
assert(presenter.foldexpr(1) == 0, "single-line sources must not create folds")

-- Explicit refresh must invalidate before asking opted-in windows to refold.
vim.api.nvim_buf_set_lines(eof_bufnr, 0, -1, false, { "first", "middle", "last" })
eof_state.sources = { source(eof_bufnr, 0, 3, true) }
vim.wo.foldmethod = "expr"
vim.wo.foldexpr = "v:lua.require'obsidian-base'.foldexpr(v:lnum, 4)"
vim.wo.foldlevel = 99
presenter.refresh_foldexpr(eof_bufnr)
assert(vim.fn.foldlevel(1) == 4, "explicit refresh did not recompute window folds")
eof_state.enabled = false
presenter.refresh_foldexpr(eof_bufnr)
assert(vim.fn.foldlevel(1) == 0, "toggle refresh retained cached window folds")

print("obsidian-base foldexpr smoke passed")
