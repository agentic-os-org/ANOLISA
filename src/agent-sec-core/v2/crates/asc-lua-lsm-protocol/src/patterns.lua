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

local function literal_parts(value)
  local parts, literal = {}, {}
  for _, token in ipairs(tokens(value, true)) do
    if token == STAR then
      parts[#parts + 1] = table.concat(literal)
      literal = {}
    else
      literal[#literal + 1] = token
    end
  end
  parts[#parts + 1] = table.concat(literal)
  return parts
end

local function affixes(path, prefix, suffix)
  return #path >= #prefix + #suffix and path:sub(1, #prefix) == prefix
    and (#suffix == 0 or path:sub(-#suffix) == suffix)
end

local function like(parts, path)
  if #parts == 1 then return path == parts[1] end
  if not affixes(path, parts[1], parts[#parts]) then return false end
  local pos, limit = #parts[1] + 1, #path - #parts[#parts]
  -- Cedar stars span separators. Taking the earliest literal occurrence leaves
  -- the most room for every later fragment. Reserve the anchored suffix.
  for i = 2, #parts - 1 do
    if parts[i] ~= "" then
      local _, last = path:find(parts[i], pos, true)
      if not last or last > limit then return false end
      pos = last + 1
    end
  end
  return true
end

for _, target in ipairs(targets) do
  for _, predicate in ipairs(not target.prefix and target.patterns or {}) do
    if predicate.cedar then predicate.parts = literal_parts(predicate.value)
    else
      predicate.tokens = segments(predicate.value)
      for i, part in ipairs(predicate.tokens) do
        predicate.tokens[i] = part == "**" and RECURSIVE or tokens(part, false)
      end
    end
  end
end

local function pattern_matches(target, path)
  if target.prefix then
    local name = path:sub(#target.path + (target.path == "/" and 1 or 2))
    return name ~= "" and not name:find("/", 1, true) and affixes(name, target.prefix, target.suffix)
  end
  local parts
  for _, predicate in ipairs(target.patterns) do
    local matches
    if predicate.cedar then
      matches = like(predicate.parts, path)
    else
      if not parts then
        parts = segments(path)
        for i, part in ipairs(parts) do parts[i] = units(part) end
      end
      matches = match_sequence(predicate.tokens, parts, RECURSIVE, component)
    end
    if matches == predicate.negated then return false end
  end
  return true
end
