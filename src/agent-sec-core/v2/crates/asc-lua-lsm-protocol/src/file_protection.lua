-- Object properties belong to this module and are shared across Lua VMs.
-- Bootstrap commands run only in the initial user namespace with MAC_ADMIN.
local errno = require("errno")
local kernel = require("kernel")
local fs = require("fs")
local audit_ok, audit = pcall(require, "audit")
local O_PATH = 2097152
-- Linux EALREADY is not exported by this baseline's errno Lua library.
local ACK = 114
local module_env = getfenv(1)
local bind_environment = assert(debug and debug.setfenv)

local function bit(value, mask)
  return value % (mask * 2) >= mask
end

local function log(reason, detail)
  if audit_ok and type(audit) == "table" and type(audit.log) == "function" then
    local ok, recorded = pcall(audit.log, {
      reason = reason, detail = detail and tostring(detail):sub(1, 256),
    })
    return ok and recorded == true
  end
  return false
end

local function key(index) return "path_" .. index end

-- Kernel object/shared caches accept scalar values, not strings or Lua tables.
-- Keep the exclusion trie in each VM; only its deterministic state IDs are shared.
local paths = {}
for index, target in ipairs(targets) do
  local states = { { children = {} } }
  for _, excluded in ipairs(target.exclusions) do
    local relative = excluded.path:sub(#target.path + (target.path == "/" and 1 or 2))
    local state = 1
    for part in relative:gmatch("[^/]+") do
      local next_state = states[state].children[part]
      if not next_state then
        next_state = #states + 1
        states[next_state] = { children = {} }
        states[state].children[part] = next_state
      end
      state = next_state
    end
    states[state].excluded = true
    if excluded.tree then states[state].subtree = true end
  end
  paths[index] = states
end

local function next_path(index, state, name)
  if state == nil or state == 0 or state == -1 then return state end
  local node = assert(paths[index][state])
  if node.subtree then return -1 end
  return node.children[name] or 0
end

local function marked(task)
  -- The dispatcher creates current before installing the module environment.
  -- Attach that environment before accessing its module-owned object cache.
  bind_environment(task, module_env)
  return task.member == true or task:group_leader().member == true
end

local function counter(key, delta)
  return assert(shared.state:incr(key, delta))
end

local function member(task)
  if task.counted ~= true then
    local first = assert(task:kvcache_incr("owned", 1))
    if first == 1 then
      counter("ever", 1)
      task.counted = true
    end
    assert(task.counted == true)
  end
  task.member = true
  assert(task.member == true)
end

local function transferred(task, epoch)
  member(task)
  -- Record the object marker first: a failed global increment overcounts the
  -- remaining cohort and therefore cannot authorize premature removal.
  if assert(task:kvcache_incr("done_" .. epoch, 1)) == 1 then
    counter("sent_" .. epoch, 1)
  end
end

local function inherited_transfer(task)
  local epoch = shared.state.epoch
  return epoch and ((task["done_" .. epoch] or 0) > 0
    or (task:group_leader()["done_" .. epoch] or 0) > 0) and epoch
end

local function with_task(pid, check)
  local task = kernel.task_from_pid(tonumber(pid))
  if task == nil then return false end
  local ok, result = pcall(check, task)
  -- task_from_pid returns a ref-counted userdata. Release it immediately so
  -- dead tasks can reach task_free instead of waiting for VM garbage collection.
  task:__gc()
  if not ok then error(result) end
  return result
end

local function in_scope()
  if marked(current) then return true end
  local pid = shared.state.root_pid
  if pid == nil then return false end
  -- PID lookups use the caller's namespace. Only the original task object
  -- can attest ancestry, even if the number now resolves to another task.
  local descendant = with_task(pid, function(root)
    return root.scope_root == true and current:is_descendant(root)
  end)
  if descendant then
    member(current)
    member(current:group_leader())
  end
  return descendant
end

local function check_in_scope(check, ...)
  if not in_scope() then return true end
  return check(...)
end

local function guarded(check, ...)
  local ok, allowed = pcall(check_in_scope, check, ...)
  if ok and allowed == true then return true end
  log(ok and allowed == false and "policy_deny" or "evaluation_error", not ok and allowed or nil)
  return false, errno.EPERM
end

local function active(index, state)
  if state == nil or state == -1 then return false end
  return state == 0 or not assert(paths[index][state]).excluded
end

local function allowed(inode, operation)
  if inode == nil then return true end
  for index, target in ipairs(targets) do
    if target[operation] and active(index, inode[key(index)]) then return false end
  end
  return true
end

local function hex(value)
  value = value:gsub("/", "")
  if value == "_" then return "" end
  assert(#value % 2 == 0 and not value:find("[^0-9a-f]"))
  return (value:gsub("..", function(pair) return string.char(tonumber(pair, 16)) end))
end

--@glob include patterns.lua
local function handle_control_command(file)
  local path = file:path()
  if type(path) ~= "string" or path:sub(1, #control + 1) ~= control .. "/" then return nil end
  if not current:userns():is_initial() or not current:capable("mac_admin") then return nil end
  local command = path:sub(#control + 2)
  local candidate = command:match("^root_candidate/(%d+)$")
  if candidate then
    assert(with_task(candidate, function(task)
      task.root_candidate = true
      return task.root_candidate == true
    end))
    return false, ACK
  end
  local root_pid = command:match("^seed_root/(%d+)$")
  if root_pid then
    root_pid = tonumber(root_pid)
    -- The Client checked the process instance after marking the candidate.
    assert(with_task(root_pid, function(task)
      assert(task.root_candidate == true)
      member(task)
      member(task:group_leader())
      task.scope_root = true
      return task.scope_root == true
    end))
    shared.state.root_pid = root_pid
    assert(shared.state.root_pid == root_pid)
    return false, ACK
  end
  local pid = command:match("^seed/(%d+)$")
  if pid then
    assert(with_task(pid, function(task)
      member(task)
      member(task:group_leader())
      return true
    end))
    return false, ACK
  end
  pid = command:match("^member/(%d+)$")
  if pid then
    return false, with_task(pid, marked) and ACK or errno.ESRCH
  end
  local epoch = command:match("^begin/([0-9a-f]+)$")
  if epoch then
    assert(#epoch == 32)
    local mapping = "handover_" .. epoch
    if shared.state[mapping] == nil then
      shared.state[mapping] = counter("epochs", 1)
    end
    local number = assert(shared.state[mapping])
    shared.state.epoch = number
    assert(shared.state.epoch == number)
    return false, ACK
  end
  pid = command:match("^sent/(%d+)$")
  if pid then
    epoch = assert(shared.state.epoch)
    assert(with_task(pid, function(task)
      assert(marked(task))
      transferred(task, epoch)
      transferred(task:group_leader(), epoch)
      return true
    end))
    return false, ACK
  end
  epoch = command:match("^complete/([0-9a-f]+)$")
  if epoch then
    epoch = assert(shared.state["handover_" .. epoch])
    assert(shared.state.epoch == epoch)
    local before = counter("ever", 0)
    local sent = counter("sent_" .. epoch, 0)
    local exited = counter("exited", 0)
    local exited_sent = counter("exit_sent_" .. epoch, 0)
    local after = counter("ever", 0)
    -- Exit publishes exit_sent before exited. This read order overcounts
    -- concurrent exits; a concurrent new member changes the ever fence.
    local complete = before == after and after + exited_sent == exited + sent
    return false, complete and ACK or errno.EAGAIN
  end
  local kind, index, relative, object = command:match("^(%a+)/(%d+)/(.-)/at/(.+)$")
  index = tonumber(index)
  assert(index and targets[index])
  local pinned = assert(fs.filp_open(hex(object), O_PATH, 0))
  local ok, failure = pcall(function()
    local inode = pinned:inode()
--@glob     if targets[index].patterns then
--@glob       if kind == "match" then return inode[key(index)] ~= 0 end
--@glob       if kind == "anchor" or kind == "root" or kind == "tag" then
--@glob         pattern_tag(inode, pinned, index, kind, object)
--@glob         return
--@glob       end
--@glob     end
    if kind == "anchor" or kind == "root" or kind == "tag" then
      local value = 1
      if kind == "anchor" then
        local fd = assert(tonumber(hex(object):match("^/proc/self/fd/(%d+)$")))
        shared.state["anchor_" .. index] = fd
        assert(shared.state["anchor_" .. index] == fd)
      elseif kind == "tag" then
        local anchor = assert(fs.filp_open("/proc/self/fd/" .. assert(shared.state["anchor_" .. index]), O_PATH, 0))
        local base = anchor:path()
        anchor:fput()
        assert(type(base) == "string")
        local path = assert(pinned:path())
        local prefix = base == "/" and "/" or base .. "/"
        assert(path:sub(1, #prefix) == prefix)
        for part in path:sub(#prefix + 1):gmatch("[^/]+") do
          value = next_path(index, value, part)
        end
      end
      -- An alias in an included range dominates an excluded alias of one inode.
      if not active(index, inode[key(index)]) then inode[key(index)] = value end
      assert(inode[key(index)] ~= nil)
    elseif kind == "slot" then
      local slot = "slot_" .. index .. "_" .. hex(relative)
      inode[slot] = true
      assert(inode[slot] == true)
    elseif kind == "stable" then
      inode["stable_" .. index] = true
      assert(inode["stable_" .. index] == true)
    else
      error("unknown bootstrap command")
    end
  end)
  pinned:fput()
--@glob   if ok and failure == true then return false, errno.ESRCH end
  if not ok then error(failure) end
  return false, ACK
end

local function inherited(parent, name, index)
  if parent == nil or type(name) ~= "string" then return nil end
  local target = targets[index]
--@glob   if target.patterns then
--@glob     local path = child_path(parent, name, index)
--@glob     if path then return pattern_matches(target,path) and 0 or -1 end
--@glob     return nil
--@glob   end
  if parent["slot_" .. index .. "_" .. name] == true then return 1 end
  if not target.tree then
    return nil
  end
  return next_path(index, parent[key(index)], name)
end

local function basename(dentry)
  local path = assert(dentry:path())
  return assert(path:match("([^/]+)$"))
end

--@create local function creation(parent, dentry)
--@create   local name = basename(dentry)
--@create   for index, target in ipairs(targets) do
--@create     if target.write and active(index, inherited(parent, name, index)) then return false end
--@create   end
--@create   return true
--@create end
local function label(inode, parent, name)
  if inode == nil then return end
  for index in ipairs(targets) do
--@glob     if targets[index].patterns then
--@glob       local path = child_path(parent, name, index)
--@glob       if path then pattern_label(inode,index,path) end
--@glob     else
    local relative = inherited(parent, name, index)
    if relative ~= nil and not active(index, inode[key(index)]) then
      inode[key(index)] = relative
      assert(inode[key(index)] ~= nil)
    end
--@glob     end
  end
end

local function tracked(check, ...)
  local ok, failure = pcall(check, ...)
  if ok then return true end
  log("evaluation_error", failure)
  return false, errno.EPERM
end

local function deletion(parent, dentry)
  local inode = dentry:backing_inode()
  if not allowed(inode, "delete") then return false end
  local name = basename(dentry)
  for index, target in ipairs(targets) do
    if target.delete and (inode and inode["stable_" .. index] == true
      or active(index, inherited(parent, name, index))) then return false end
  end
  return true
end

local function rename(old_dir, old, new_dir, new)
  local source, destination = old:backing_inode(), new:backing_inode()
  local old_name, new_name = basename(old), basename(new)
  for index, target in ipairs(targets) do
    local from = active(index, source and source[key(index)])
    local into = active(index, inherited(new_dir, new_name, index))
    if target.delete then
      if source and source["stable_" .. index] == true then return false end
      if destination and (destination["stable_" .. index] == true
        or active(index, destination[key(index)])) then return false end
      if from and (not target.tree or not into) then return false end
      if active(index, inherited(old_dir, old_name, index)) and not into then return false end
    end
    -- Lua-LSM has no post-rename callback. Keep labels valid across allowed
    -- operations by rejecting moves that change a write/read protection scope.
    if (target.write or target.read) and target.tree and from ~= into then return false end
    if target.tree and from and into and #target.exclusions > 0
      and (old_name ~= new_name or old_dir[key(index)] ~= new_dir[key(index)]) then return false end
  end
  return true
end

local function mount_allowed(path)
  local inode = assert(path:dentry():backing_inode())
  for index, target in ipairs(targets) do
--@glob     if target.patterns and inode[dir_key(index)] then return false end
    if (target.read or target.write or target.delete)
      and (inode["stable_" .. index] == true or active(index, inode[key(index)])) then
      return false
    end
  end
  return true
end

return {
  name = name,
  author = "AgentSecCore",
  license = "GPL-2.0",
  description = "AgentSecCore file and process-tree protection",
  version = 1,
  file_open = function(file)
    local ok, result, code = pcall(handle_control_command, file)
    if not ok then
      if not log("bootstrap_error", result) and type(kernel.pr_warn) == "function" then
        pcall(kernel.pr_warn, name .. ": bootstrap_error: " .. tostring(result):sub(1, 256))
      end
      local checked, scoped = pcall(in_scope)
      if not checked or scoped then return false, errno.EIO end
      return true
    end
    if result ~= nil then return result, code end
    return guarded(function()
      local inode = assert(file:inode())
      return (not (file:fmode("read") or file:fmode("exec")) or allowed(inode, "read"))
        and (not file:fmode("write") or allowed(inode, "write"))
    end)
  end,
  file_permission = function(file, mask)
    return guarded(function()
      local inode = assert(file:inode())
      return (not bit(mask, 4) or allowed(inode, "read"))
        and (not bit(mask, 2) or allowed(inode, "write"))
    end)
  end,
  mmap_file = function(file, reqprot, prot, flags)
    if file == nil then return true end
    return guarded(function()
      local inode = assert(file:inode())
      return (prot == 0 or allowed(inode, "read"))
        and (not (bit(prot, 2) and bit(flags, 1)) or allowed(inode, "write"))
    end)
  end,
  file_mprotect = function(vma, reqprot, prot)
    return guarded(function()
      local file = vma:file()
      if file == nil then return true end
      local ok, result = pcall(function()
        local inode = assert(file:inode())
        return (prot == 0 or allowed(inode, "read"))
          and (not (bit(prot, 2) and vma:mapping("shared")) or allowed(inode, "write"))
      end)
      file:fput()
      if not ok then error(result) end
      return result
    end)
  end,
  inode_setattr = function(idmap, dentry, attr)
    -- ABI v1 kernels also expose the legacy (dentry, attr) argument pair.
    if attr == nil then dentry, attr = idmap, dentry end
    return guarded(function()
      assert(type(attr) == "table")
      return attr.size == nil or allowed(dentry:backing_inode(), "write")
    end)
  end,
--@create   inode_create = function(dir, dentry) return guarded(creation, dir, dentry) end,
--@create   inode_mkdir = function(dir, dentry) return guarded(creation, dir, dentry) end,
  inode_unlink = function(dir, dentry) return guarded(deletion, dir, dentry) end,
  inode_rmdir = function(dir, dentry) return guarded(deletion, dir, dentry) end,
  sb_mount = function(device, path) return guarded(mount_allowed, path) end,
  move_mount = function(source, destination)
    return guarded(function() return mount_allowed(source) and mount_allowed(destination) end)
  end,
  inode_rename = function(old_dir, old, new_dir, new)
--@glob     local moving, failure = guarded(pattern_move, old_dir, old, new_dir, new)
--@glob     if not moving then return false, failure end
    local ok, code = guarded(rename, old_dir, old, new_dir, new)
    if not ok then return ok, code end
    -- Tag incoming objects before commit so an existing outside hard link cannot
    -- bypass protection. Failed namespace changes can retain conservative labels.
    return tracked(label, old:backing_inode(), new_dir, basename(new))
  end,
  inode_link = function(old, new_dir, new)
--@glob     local ok, code = guarded(pattern_link, old, new_dir, new)
--@glob     if not ok then return false, code end
    return tracked(label, old:backing_inode(), new_dir, basename(new))
  end,
  inode_init_security = function(inode, parent, name)
    return tracked(label, inode, parent, name)
  end,
  bprm_check_security = function(binprm)
    -- Preserve membership when a non-leader thread becomes the leader on exec.
    return guarded(function()
      local epoch = inherited_transfer(current)
      if current:group_leader().scope_root == true then current.scope_root = true end
      member(current)
      if epoch then transferred(current, epoch) end
      return allowed(assert(binprm:file():inode()), "read")
    end)
  end,
  task_alloc = function(child)
    return guarded(function()
      member(child)
      local epoch = inherited_transfer(current)
      if epoch then transferred(child, epoch) end
      return true
    end)
  end,
  task_free = function(task)
    if task.counted ~= true then return end
    local ok = pcall(function()
      local epoch = shared.state.epoch
      if epoch and (task["done_" .. epoch] or 0) > 0 then
        counter("exit_sent_" .. epoch, 1)
      end
      counter("exited", 1)
    end)
    if not ok then log("evaluation_error") end
  end,
}
