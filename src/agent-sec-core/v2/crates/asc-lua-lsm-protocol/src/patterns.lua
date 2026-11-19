-- Filename bytes stay in Lua. Never pass authored text to a Lua pattern evaluator.
local function units(s)
  local out, i = {}, 1
  while i <= #s do
    local j = i + 1
    local b = s:byte(i)
    if b >= 194 and b <= 244 then
      local n = b < 224 and 2 or (b < 240 and 3 or 4)
      while j < i + n and j <= #s and s:byte(j) >= 128 and s:byte(j) < 192 do j = j + 1 end
    end
    out[#out + 1] = s:sub(i, j - 1)
    i = j
  end
  return out
end

local function segments(s)
  local out = {}
  for part in s:gmatch("[^/]+") do out[#out + 1] = part end
  return out
end

local STAR, ONE, RECURSIVE = {}, {}, {}
local function tokens(value, cedar)
  local result, escaped = {}, false
  for _, ch in ipairs(units(value)) do
    if cedar and not escaped and ch == "\\" then escaped = true
    else
      result[#result + 1] = not escaped and ch == "*" and STAR
        or (not cedar and ch == "?" and ONE) or ch
      escaped = false
    end
  end
  assert(not escaped)
  return result
end

-- Iterative wildcard matching with one retry position, shared by string and segment matching.
local function match_sequence(pattern, input, star, equal)
  local p, i, last, retry = 1, 1
  while i <= #input do
    if pattern[p] == star then
      last, retry, p = p, i, p + 1
    elseif pattern[p] and equal(pattern[p], input[i]) then
      p, i = p + 1, i + 1
    elseif last then
      retry = retry + 1
      p, i = last + 1, retry
    else return false end
  end
  while pattern[p] == star do p = p + 1 end
  return p > #pattern
end
local function same(a, b) return a == ONE or a == b end
local function component(pattern, input) return match_sequence(pattern, input, STAR, same) end

for _, target in ipairs(targets) do
  for _, predicate in ipairs(target.patterns or {}) do
    if predicate.cedar then predicate.tokens = tokens(predicate.value, true)
    else
      predicate.tokens = segments(predicate.value)
      for i, part in ipairs(predicate.tokens) do
        predicate.tokens[i] = part == "**" and RECURSIVE or tokens(part, false)
      end
    end
  end
end

local function pattern_matches(target, path)
  local chars, parts
  for _, predicate in ipairs(target.patterns) do
    local matches
    if predicate.cedar then
      chars = chars or units(path)
      matches = match_sequence(predicate.tokens, chars, STAR, same)
    else
      if not parts then
        parts = segments(path)
        for i, part in ipairs(parts) do parts[i] = units(part) end
      end
      matches = match_sequence(predicate.tokens, parts, RECURSIVE, component)
    end
    if matches == predicate.negated then return false end
  end
  for _, excluded in ipairs(target.exclusions) do
    if path == excluded.path or (excluded.tree and path:sub(1, #excluded.path+1) == excluded.path.."/") then return false end
  end
  return true
end

-- inode_init_security supplies a parent inode and a basename. Save directory paths in
-- numeric object-cache entries (the kernel does not store strings). Publish length last.
local function dir_key(index) return "globdir_"..index end
local function remember_dir(inode, index, path)
  if not targets[index].tree or not inode:filetype("dir") then return end
  assert(#path <= 4096)
  path = path:sub(#targets[index].path+1)
  local prefix = dir_key(index)
  for i = 1, #path, 3 do
    local a,b,c = path:byte(i,i+2)
    local n = a * 65536 + (b or 0) * 256 + (c or 0)
    local k = prefix.."_"..i
    inode[k] = n
    assert(inode[k] == n)
  end
  inode[prefix] = #path
  assert(inode[prefix] == #path)
end
local function child_path(parent, name, index)
  if not parent or type(name) ~= "string" then return nil end
  if parent["slot_"..index.."_"..name] then return targets[index].path end
  local prefix = dir_key(index)
  local length = parent[prefix]
  if not length then return nil end
  local parts = {}
  for i = 1, length, 3 do
    local n = assert(parent[prefix.."_"..i])
    local c = n % 256
    n = (n - c) / 256
    local b = n % 256
    parts[#parts+1] = string.char((n-b)/256,b,c)
  end
  local path = targets[index].path..table.concat(parts):sub(1,length)
  return (path == "/" and "" or path).."/"..name
end
local function pattern_label(inode, index, path)
  remember_dir(inode, index, path)
  if pattern_matches(targets[index], path) then
    inode[key(index)] = 0
    assert(inode[key(index)] == 0)
  end
end
local function pattern_tag(inode, pinned, index, kind, object)
  local path = targets[index].path
  if kind == "anchor" then
    local fd = assert(tonumber(hex(object):match("^/proc/self/fd/(%d+)$")))
    shared.state["anchor_"..index] = fd
    assert(shared.state["anchor_"..index] == fd)
  elseif kind == "tag" then
    local anchor = assert(fs.filp_open("/proc/self/fd/"..assert(shared.state["anchor_"..index]), O_PATH, 0))
    local base = anchor:path()
    anchor:fput()
    local prefix = base == "/" and "/" or assert(base).."/"
    local actual = assert(pinned:path())
    assert(actual:sub(1,#prefix) == prefix)
    path = (path == "/" and "/" or path.."/")..actual:sub(#prefix+1)
  end
  pattern_label(inode, index, path)
end

local function pattern_move(old_dir, old, new_dir, new)
  local source, dest = old:backing_inode(), new:backing_inode()
  for index, target in ipairs(targets) do
    if target.patterns then
      local dir = dir_key(index)
      -- Moving a populated directory would invalidate its descendants' stored paths.
      if (source and source:filetype("dir") or dest and dest:filetype("dir"))
        and (old_dir[dir] or new_dir[dir] or source and source["stable_"..index]
          or dest and dest["stable_"..index]) then return false end
    end
  end
  return true
end

local function pattern_link(old, new_dir, new)
  for index, target in ipairs(targets) do
    if target.patterns and (target.read or target.write) and old:backing_inode()[key(index)] ~= 0 then
      local path = child_path(new_dir, assert(new:path():match("([^/]+)$")), index)
      -- A mapped file cannot acquire read/write protection safely via a new alias.
      if path and pattern_matches(target,path) then return false end
    end
  end
  return true
end
