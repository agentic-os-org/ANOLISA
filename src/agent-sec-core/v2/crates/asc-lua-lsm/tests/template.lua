-- Run from asc-lua-lsm: lua tests/template.lua ../asc-lua-lsm-protocol/src/file_protection.lua
local template = assert(io.open(arg[1], "r")):read("*a")
template = template:gsub("\n%-%-@create ", "\n")
local events, objects, tasks = {}, {}, {}
-- Real kernel objects are userdata; these mock objects carry ordinary Lua tables.
debug.setfenv = function(object) return object end
math = nil
local cached = setmetatable({}, { __mode = "k" })
local function cache(object)
  if cached[object] then return object end
  local values = {}
  for key, value in pairs(object) do
    if type(value) ~= "function" then
      values[key], object[key] = value, nil
    end
  end
  cached[object] = true
  return setmetatable(object, {
    __index = function(_, key) return values[key] end,
    __newindex = function(self, key, value)
      if type(value) == "function" then rawset(self, key, value); return end
      assert(value == nil or type(value) == "number" or type(value) == "boolean", "unsupported kernel cache value")
      values[key] = value
    end,
  })
end
local admin = { member = false }
function admin:group_leader() return self end
function admin:userns() return { is_initial = function() return true end } end
function admin:capable() return true end
function admin:is_descendant() return false end
local root = { member = false }
function root:group_leader() return self end
function root:is_descendant() return true end
tasks[42] = root
current = admin
shared = { state = {} }
local errno = { EPERM = 1, ESRCH = 3, EIO = 5, EAGAIN = 11 }
local ACK = 114
local function incr(object, name, delta)
  object[name] = (object[name] or 0) + delta
  return object[name]
end
shared.state.incr = incr
cache(shared.state)
local function task_methods(task)
  task.kvcache_incr = incr
  task.__gc = function() end
  return cache(task)
end
task_methods(root)
task_methods(admin)
local fs = { filp_open = function(path) return objects[path] end }
package.preload.errno = function() return errno end
package.preload.kernel = function() return { task_from_pid = function(pid) return tasks[pid] end } end
package.preload.fs = function() return fs end
package.preload.audit = function() return { log = function(event) events[#events + 1] = event.reason end } end
local function file(inode, path, read, write)
  if inode then cache(inode) end
  return {
    inode = function() return inode end,
    path = function() return path end,
    fmode = function(_, mode) return mode == "read" and read == true or mode == "write" and write == true end,
    fput = function() end,
  }
end
local function hex(value)
  if value == "" then return "_" end
  return (value:gsub(".", function(ch) return string.format("%02x", ch:byte()) end))
end
local function load_module()
  local prefix = [[
local name, control, root_pid = "asc_test_r1", "/control", 42
local targets = {
  { path = "/exact", tree = false, read = true, write = false, delete = false, exclusions = {} },
  { path = "/tree", tree = true, read = false, write = true, delete = true,
    exclusions = { { path = "/tree/cache", tree = true }, { path = "/tree/entry", tree = false } } },
  { path = "/write", tree = false, read = false, write = true, delete = false, exclusions = {} },
  { path = "/delete", tree = false, read = false, write = false, delete = true, exclusions = {} },
  { path = "/read-tree", tree = true, read = true, write = false, delete = false, exclusions = {} },
}
]]
  local loader = loadstring or load
  return assert(loader(prefix .. template))()
end
local module = load_module()
local hook_count = 0
for _, value in pairs(module) do if type(value) == "function" then hook_count = hook_count + 1 end end
assert(hook_count == 17)
local function command(command)
  current = admin
  local allowed, code = module.file_open(file({}, "/control/" .. command))
  assert(allowed == false and code == ACK, command .. ": " .. tostring(code))
end
local function tag(kind, index, relative, inode)
  local handle = "/proc/self/fd/" .. tostring(10 + index)
  local roots = { "/exact", "/tree", "/write", "/delete", "/read-tree" }
  local object_path = roots[index] .. (relative == "" and "" or "/" .. relative)
  if kind == "tag" then
    if relative == "" then
      kind = "anchor"
    else
      -- Keep the root descriptor available while tagging descendants.
      handle = "/proc/self/fd/99"
    end
    relative = ""
  end
  objects[handle] = file(inode, object_path)
  command(kind .. "/" .. index .. "/" .. hex(relative) .. "/at/" .. hex(handle))
end
command("seed/42")
local exact, tree, protected, excluded, entry, write, deletion = {}, {}, {}, {}, {}, {}, {}
tag("tag", 1, "", exact)
tag("tag", 2, "", tree)
tag("tag", 2, "data", protected)
tag("tag", 2, "cache", excluded)
tag("tag", 2, "entry", entry)
tag("tag", 3, "", write)
tag("tag", 4, "", deletion)
local parent = {}
tag("slot", 4, "delete", parent)
tag("stable", 4, "", deletion)
command("activate")
current = root
assert(module.file_open(file(exact, "/renamed-hard-link", true)) == false)
assert(module.file_permission(file(exact), 4) == false)
assert(module.file_permission(file(exact), 2) == true)
assert(module.mmap_file(file(exact), 1, 1, 2) == false)
assert(module.mmap_file(file(write), 2, 2, 1) == false)
assert(module.mmap_file(file(write), 2, 2, 2) == true)
assert(module.mmap_file(nil, 1, 1, 1) == true)
assert(module.file_open(file(write, "/write", true, false)) == true)
assert(module.file_open(file(write, "/write", false, true)) == false)
assert(module.file_permission(file(protected), 2) == false)
assert(module.file_permission(file(excluded), 2) == true)
assert(module.file_permission(file(entry), 2) == true)
local function dentry(inode, path)
  if inode then cache(inode) end
  return { backing_inode = function() return inode end, path = function() return path end }
end
for _, hook in ipairs({module.inode_create, module.inode_mkdir}) do
  assert(hook(tree, dentry(nil, "/tree/new")) == false)
  assert(hook(tree, dentry(nil, "/tree/entry")) == true)
  assert(hook(excluded, dentry(nil, "/tree/cache/new")) == true)
  assert(hook(parent, dentry(nil, "/delete")) == true)
end
assert(module.inode_unlink(parent, dentry(write, "/write")) == true)
assert(module.inode_unlink(parent, dentry(deletion, "/delete")) == false)
assert(module.inode_rmdir({}, dentry(tree, "/tree")) == false)
local function path(inode) return { dentry = function() return dentry(inode, "/mount") end } end
assert(module.sb_mount(nil, path(tree)) == false)
assert(module.move_mount(path({}), path(tree)) == false)
assert(module.sb_mount(nil, path({})) == true)
assert(module.inode_setattr(nil, dentry(write, "/write"), { size = 0 }) == false)
assert(module.inode_setattr(nil, dentry(write, "/write"), { mode = 420 }) == true)
assert(module.inode_setattr(dentry(write, "/write"), { size = 0 }) == false)
assert(module.inode_setattr(dentry(write, "/write"), { mode = 420 }) == true)
local created, excluded_child = cache({}), cache({})
assert(module.inode_init_security(created, tree, "created"))
assert(module.inode_init_security(excluded_child, excluded, "new"))
assert(module.file_permission(file(created), 2) == false)
assert(module.file_permission(file(excluded_child), 2) == true)
local incoming = {}
assert(module.inode_link(dentry(incoming, "/outside-alias"), tree, dentry(nil, "/tree/incoming")))
assert(module.file_permission(file(incoming), 2) == false)
local thread = task_methods({ group_leader = function() return root end })
current = thread
assert(module.bprm_check_security({ file = function() return file({}) end }))
assert(thread.member == true)
function thread:group_leader() return self end
current = root
local replacement = cache({})
assert(module.inode_init_security(replacement, parent, "delete"))
assert(module.inode_unlink(parent, dentry(replacement, "/delete")) == false)
local child = task_methods({ })
function child:group_leader() return self end
function child:is_descendant() return false end
assert(module.task_alloc(child))
assert(child.member == true)
-- Reparenting and root exit do not remove the inherited object property.
tasks[42] = nil
current = child
assert(module.file_permission(file(exact), 4) == false)
local grandchild = task_methods({})
function grandchild:group_leader() return self end
assert(module.task_alloc(grandchild))
assert(grandchild.member == true)
tasks[43], tasks[44], tasks[45] = child, thread, grandchild
module.task_free(root)
local epoch = string.rep("a", 32)
command("begin/" .. epoch)
current = admin
local ok, code = module.file_open(file({}, "/control/complete/" .. epoch))
assert(ok == false and code == errno.EAGAIN)
command("member/43")
command("sent/43")
current = child
local late = task_methods({ group_leader = function(self) return self end })
assert(module.task_alloc(late))
command("sent/44")
command("sent/45")
command("complete/" .. epoch)
command("sent/43")
command("complete/" .. epoch)
command("begin/" .. string.rep("b", 32))
current = admin
ok, code = module.file_open(file({}, "/control/complete/" .. string.rep("b", 32)))
assert(ok == false and code == errno.EAGAIN)
command("begin/" .. epoch)
local original_incr, injected = shared.state.incr, false
shared.state.incr = function(dict, name, delta)
  if name == "exited" and delta == 0 and not injected then
    injected = true
    local caller = current
    current = child
    assert(module.task_alloc(task_methods({ group_leader = function(self) return self end })))
    current = caller
  end
  return original_incr(dict, name, delta)
end
current = admin
ok, code = module.file_open(file({}, "/control/complete/" .. epoch))
assert(ok == false and code == errno.EAGAIN)
shared.state.incr = original_incr
command("complete/" .. epoch)
module.task_free(late)
command("complete/" .. epoch)
current = child
-- A second VM sees the same per-object properties and shared activation state.
module = load_module()
assert(module.file_permission(file(exact), 4) == false)
local fault = { inode = function() error("evaluation failed") end }
assert(module.file_permission(fault, 4) == false)
assert(events[#events] == "evaluation_error")
assert(module.file_permission({ inode = function() return nil end }, 4) == false)
assert(module.bprm_check_security({ file = function() return file(exact) end }) == false)
assert(module.file_permission(file(exact), 4) == false)
assert(events[#events] == "policy_deny")
local vma = { file = function() return file(exact) end, mapping = function() return false end }
assert(module.file_mprotect(vma, 1, 1) == false)
current = admin
assert(module.file_permission(file(exact), 4) == true)
assert(module.file_permission(fault, 4) == true)
print("Lua-LSM template: object aliases, independent operations, exclusions, inheritance, VM reload and error policy passed")
