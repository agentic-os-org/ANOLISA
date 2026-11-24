-- Only task membership uses module-owned kernel object properties.
-- Bootstrap commands run only in the initial user namespace with MAC_ADMIN.
local errno = require("errno")
local kernel = require("kernel")
--@glob local fs = require("fs")
local audit_ok, audit = pcall(require, "audit")
--@glob local O_PATH = 2097152
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

--@glob include patterns.lua

local function below(path, base)
  return path == base or (base == "/" and path:sub(1, 1) == "/")
    or (path:sub(1, #base) == base and path:sub(#base + 1, #base + 1) == "/")
end

local function matches(target, path)
  if path ~= target.path and not (target.tree and below(path, target.path)) then return false end
  for _, excluded in ipairs(target.exclusions) do
    if path == excluded.path or (excluded.tree and below(path, excluded.path)) then return false end
  end
--@glob   if target.patterns then return pattern_matches(target, path) end
  return true
end

-- A file supplies a namespace path. A dentry supplies a filesystem-relative
-- path; the Client records the corresponding mount roots when registering.
local function selected(target, path, filesystem, ancestors)
  local function check(logical)
    return matches(target, logical) or (ancestors and below(target.path, logical))
  end
  if not filesystem and check(path) then return true end
  for _, view in ipairs(target.views or {}) do
    if view.fs == filesystem then
      if view.logical and below(path, view.base) then
        local suffix = path:sub(#view.base + 1)
        if view.base == "/" and suffix ~= "" then suffix = "/" .. suffix end
        local logical = suffix == "" and view.logical
          or (view.logical == "/" and suffix or view.logical .. suffix)
        if check(logical) then return true end
      elseif ancestors and below(view.base, path) then
        return true
      end
    end
  end
  return false
end

local function allowed(path, operation, filesystem, ancestors)
  assert(type(path) == "string")
  for _, target in ipairs(targets) do
    if (not operation or target[operation]) and selected(target, path, filesystem, ancestors) then
      return false
    end
  end
  return true
end

local function file_allowed(file, operation)
  local path = assert(file:path())
  if not allowed(path, operation) then return false end
  -- d_path appends this suffix to an unlinked file still held by a descriptor.
  return path:sub(-10) ~= " (deleted)" or allowed(path:sub(1, -11), operation)
end

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
--@glob   local index, fd = command:match("^match/(%d+)/(%d+)$")
--@glob   if index then
--@glob     local target = assert(targets[tonumber(index)])
--@glob     local pinned = assert(fs.filp_open("/proc/self/fd/" .. fd, O_PATH, 0))
--@glob     local ok, match = pcall(function() return selected(target, assert(pinned:path())) end)
--@glob     pinned:fput()
--@glob     if not ok then error(match) end
--@glob     return false, match and ACK or errno.ESRCH
--@glob   end
  error("unknown control command")
end

local function filesystem(path)
  return assert(path:vfsmount():superblock():fstype_name())
end

local function entry_allowed(parent, entry, operation, ancestors)
  return allowed(assert(entry:path(true)), operation, filesystem(parent), ancestors)
end

local function mount_allowed(path)
  return entry_allowed(path, path:dentry(), nil, true)
end

local function rename(old_dir, old, new, flags)
  local fsname = filesystem(old_dir)
  local from, into = assert(old:path(true)), assert(new:path(true))
  local overwritten = new:backing_inode()
  local exchange = bit(flags, 2)
  for _, target in ipairs(targets) do
    local source = selected(target, from, fsname)
    local dest = selected(target, into, fsname)
    if target.write and (dest or exchange and source) then return false end
    if target.delete and overwritten and dest then return false end
    if source ~= dest and (target.read or target.write or source or exchange) then return false end
    -- Preserve protected root names and their ancestors when moving directories.
    if selected(target, from, fsname, true) and (not source or not target.tree) then return false end
    if selected(target, into, fsname, true) and not dest then return false end
    if target.tree and (target.patterns or #target.exclusions > 0) and old:backing_inode():filetype("dir") then
      -- A directory name need not itself match a predicate on its descendants.
      for _, view in ipairs(target.views or {}) do
        if view.logical and view.fs == fsname and (below(from, view.base) or below(into, view.base)) then return false end
      end
    end
  end
  return true
end

local function link(old, parent, new)
  local fsname = filesystem(parent)
  local from, into = assert(old:path(true)), assert(new:path(true))
  for _, target in ipairs(targets) do
    local dest = selected(target, into, fsname)
    if target.write and dest then return false end
    if (target.read or target.write) and selected(target, from, fsname) ~= dest then return false end
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
      return (not (file:fmode("read") or file:fmode("exec")) or file_allowed(file, "read"))
        and (not file:fmode("write") or file_allowed(file, "write"))
    end)
  end,
  file_permission = function(file, mask)
    return guarded(function()
      return (not bit(mask, 4) or file_allowed(file, "read"))
        and (not bit(mask, 2) or file_allowed(file, "write"))
    end)
  end,
  mmap_file = function(file, reqprot, prot, flags)
    if file == nil then return true end
    return guarded(function()
      return (prot == 0 or file_allowed(file, "read"))
        and (not (bit(prot, 2) and bit(flags, 1)) or file_allowed(file, "write"))
    end)
  end,
  file_mprotect = function(vma, reqprot, prot)
    return guarded(function()
      local file = vma:file()
      if file == nil then return true end
      local ok, result = pcall(function()
        return (prot == 0 or file_allowed(file, "read"))
          and (not (bit(prot, 2) and vma:mapping("shared")) or file_allowed(file, "write"))
      end)
      file:fput()
      if not ok then error(result) end
      return result
    end)
  end,
  path_truncate = function(path)
    return guarded(function() return entry_allowed(path, path:dentry(), "write") end)
  end,
  file_truncate = function(file) return guarded(file_allowed, file, "write") end,
--@create   path_symlink = function(dir, entry) return guarded(entry_allowed, dir, entry, "write") end,
--@create   path_mknod = function(dir, entry) return guarded(entry_allowed, dir, entry, "write") end,
--@create   path_mkdir = function(dir, entry) return guarded(entry_allowed, dir, entry, "write") end,
  path_unlink = function(dir, entry) return guarded(entry_allowed, dir, entry, "delete") end,
  path_rmdir = function(dir, entry) return guarded(entry_allowed, dir, entry, "delete", true) end,
  sb_mount = function(device, path) return guarded(mount_allowed, path) end,
  move_mount = function(source, destination)
    return guarded(function() return mount_allowed(source) and mount_allowed(destination) end)
  end,
  path_rename = function(old_dir, old, new_dir, new, flags)
    return guarded(rename, old_dir, old, new, flags)
  end,
  path_link = function(old, dir, new) return guarded(link, old, dir, new) end,
  bprm_check_security = function(binprm)
    -- Preserve membership when a non-leader thread becomes the leader on exec.
    return guarded(function()
      local epoch = inherited_transfer(current)
      if current:group_leader().scope_root == true then current.scope_root = true end
      member(current)
      if epoch then transferred(current, epoch) end
      return file_allowed(assert(binprm:file()), "read")
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
