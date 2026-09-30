-- Verifies display-cell truncation and bounds full-input character counting.
local script = debug.getinfo(1, "S").source:sub(2)
vim.opt.runtimepath:prepend(vim.fs.dirname(vim.fs.dirname(script)))

local presenter = require("obsidian-base.presenter")

---Find a private helper without adding a production API solely for testing.
---@param fn function
---@param wanted string
---@return function
local function upvalue(fn, wanted)
  local index = 1
  while true do
    local name, value = debug.getupvalue(fn, index)
    if not name then error("missing presenter helper: " .. wanted, 0) end
    if name == wanted then return value end
    index = index + 1
  end
end

-- Table columns have a minimum width of three; exercise zero/one directly.
local truncate = upvalue(upvalue(presenter.project_result, "table_lines"), "truncate")
local cases = {
  { text = "abcdef", expected = { "", "…", "a…", "ab…", "abc…", "abcd…", "abcdef", "abcdef" } },
  { text = "éöñà", expected = { "", "…", "é…", "éö…", "éöñà", "éöñà" } },
  { text = "ééé", expected = { "", "…", "é…", "ééé", "ééé" } },
  { text = "界界界", expected = { "", "…", "…", "界…", "界…", "界界…", "界界界", "界界界" } },
}
for _, case in ipairs(cases) do
  for index, expected in ipairs(case.expected) do
    local width = index - 1
    local actual = truncate(case.text, width)
    assert(actual == expected, string.format("truncate(%q, %d): expected %q, got %q", case.text, width, expected, actual))
    assert(vim.fn.strdisplaywidth(actual) <= width, "truncation exceeded its display-cell budget")
  end
end
assert(truncate("", 0) == "" and truncate("", 1) == "", "empty text changed")
assert(truncate(nil, 4) == "" and truncate(12345, 4) == "123…", "text coercion changed")

-- Count calls rather than elapsed time so the performance regression is deterministic.
local strchars, count = vim.fn.strchars, 0
vim.fn.strchars = function(text)
  count = count + 1
  return strchars(text)
end
for _, bytes in ipairs({ 1024, 10240 }) do
  count = 0
  local lines = presenter.project_result({
    columns = { { label = "Value" } },
    preview_rows = { { cells = { { text = string.rep("x", bytes) } } } },
  }, 80, "")
  assert(lines[3][1][1] == "│ " .. string.rep("x", 73) .. "… │ ", "long-cell projection changed")
  assert(count == 1, "a truncated cell must count its characters only once")
end
count = 0
assert(truncate("abcdef", 0) == "" and truncate("abcdef", 1) == "…" and truncate("abcdef", 6) == "abcdef")
assert(count == 0, "truncation fast paths should not count characters")
vim.fn.strchars = strchars

print("obsidian-base presenter smoke passed")
