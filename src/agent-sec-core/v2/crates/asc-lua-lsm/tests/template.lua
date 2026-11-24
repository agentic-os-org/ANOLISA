-- Run from asc-lua-lsm: lua tests/template.lua ../asc-lua-lsm-protocol/src/file_protection.lua
local template = assert(io.open(arg[1], "r")):read("*a")
local patterns = assert(io.open(arg[1]:gsub("file_protection%.lua$", "patterns.lua"), "r")):read("*a")
template = template:gsub("%-%-@glob include patterns.lua", function() return patterns end)
template = template:gsub("\n%-%-@glob ", "\n")
template = template:gsub("\n%-%-@create ", "\n")
local events, objects, tasks = {}, {}, {}
-- Real kernel objects are userdata; these mock objects carry ordinary Lua tables.
debug.setfenv = function(object) return object end
math = nil
local cached = setmetatable({}, { __mode = "k" })
local function cache(object)
  if cached[object] then return object end
  object.filetype = function(self, kind) return kind == "dir" and self.directory == true end
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
  { path = "/later/dir/secret", read = true, exclusions = {} },
  { path = "/glob", tree = true, read = true, exclusions = {}, patterns = {
    { value = "/glob/**/?.txt", negated = false } } },
  { path = "/like", tree = true, read = true, exclusions = {}, patterns = {
    { value = [=[/like/*literal\*?]=], cedar = true, negated = false },
    { value = "*skip*", cedar = true, negated = true } } },
  { path = "/simple", tree = true, read = true, exclusions = {}, prefix = "pre", suffix = ".txt",
    patterns = { { value = "/simple/pre*.txt", negated = false } } },
  { path = "/chain", tree = true, read = true, exclusions = {}, patterns = {
    { value = "/chain/a*ab*ab", cedar = true, negated = false } } },
}
for _, target in ipairs(targets) do
  target.views = { { base = target.path, logical = target.path, fs = "ext4" } }
end
targets[2].views[2] = { base = "/inner", logical = "/tree", fs = "tmpfs" }
targets[2].views[3] = { base = "/bind", logical = "/tree" }
targets[2].views[4] = { base = "/outer/mounted", fs = "fuse" }
]]
  local loader = loadstring or load
  return assert(loader(prefix .. template))()
end
local module = load_module()
local hook_count = 0
for _, value in pairs(module) do if type(value) == "function" then hook_count = hook_count + 1 end end
assert(hook_count == 18)
local function command(command)
  current = admin
  local allowed, code = module.file_open(file({}, "/control/" .. command))
  assert(allowed == false and code == ACK, command .. ": " .. tostring(code))
end
local function process(ancestor)
  return task_methods({
    group_leader = function(self) return self end,
    is_descendant = function(_, root_task) return root_task == ancestor end,
  })
end
command("root_candidate/42")
-- Reuse between identity verification and publication must not bind this task.
tasks[42] = process(nil)
local _, acknowledgement = module.file_open(file({}, "/control/seed_root/42"))
assert(acknowledgement ~= ACK and not tasks[42].member and not shared.state.root_pid)
tasks[42] = root
command("seed_root/42")
local exact = {}
current = root
assert(not module.file_permission(file(exact, "/exact"), 4))
-- These existing descendants have never received a seed command.
local reader, forker, forked = process(root), process(root), process(nil)
current = reader
assert(module.file_permission(file(exact, "/exact"), 4) == false and reader.member)
current = forker
assert(module.task_alloc(forked) and forker.member and forked.member)
-- A numeric collision (including in another PID namespace) grants no ancestry.
local unrelated_root = process(nil)
tasks[42] = unrelated_root
current = process(unrelated_root)
assert(module.file_permission(file(exact, "/exact"), 4) == true and not current.member)
current = forked
assert(module.file_permission(file(exact, "/exact"), 4) == false)
tasks[42] = root
for _, task in ipairs({reader, forker, forked}) do module.task_free(task) end
current = root
-- The same name remains protected when the kernel supplies a fresh object.
assert(not module.file_permission(file({}, "/exact"), 4))
assert(module.file_open(file(exact, "/outside-alias", true)))
assert(not module.file_open(file(exact, "/exact", true)))
assert(not module.file_permission(file(exact, "/exact (deleted)"), 4))
assert(module.file_permission(file(exact, "/exact"), 2))
assert(not module.mmap_file(file(exact, "/exact"), 1, 1, 2))
assert(not module.mmap_file(file({}, "/write"), 2, 2, 1))
assert(module.mmap_file(file({}, "/write"), 2, 2, 2))
assert(module.mmap_file(nil, 1, 1, 1))
assert(module.file_open(file({}, "/write", true)))
assert(not module.file_open(file({}, "/write", false, true)))
for _, name in ipairs({"/tree/data", "/tree/new/sub/file", "/tree/entry/inside", "/bind/data"}) do
  assert(not module.file_permission(file({}, name), 2), name)
end
for _, name in ipairs({"/trees/data", "/tree/cache/new", "/tree/entry", "/bind/cache/new"}) do
  assert(module.file_permission(file({}, name), 2), name)
end
local function dentry(name, kind)
  return { path = function(_, raw) assert(raw); return name end,
    backing_inode = function() return kind and { filetype = function(_, k) return k == kind end } end }
end
local function mount(name, fsname)
  local sb = { fstype_name = function() return fsname or "ext4" end }
  return { dentry = function() return dentry(name, "dir") end,
    vfsmount = function() return { superblock = function() return sb end } end }
end
local root_mount, ram = mount("/"), mount("/", "tmpfs")
for _, operation in ipairs({"path_mknod", "path_symlink", "path_mkdir"}) do
  local hook = assert(module[operation])
  assert(not hook(root_mount, dentry("/tree/new")))
  assert(hook(root_mount, dentry("/tree/entry")))
  assert(hook(root_mount, dentry("/tree/cache/new")))
  assert(not hook(ram, dentry("/inner/new")))
  assert(hook(root_mount, dentry("/inner/new")))
end
assert(module.path_unlink(root_mount, dentry("/write")))
assert(not module.path_unlink(root_mount, dentry("/delete")))
assert(not module.path_rmdir(root_mount, dentry("/tree")))
assert(not module.path_truncate(mount("/write")))
assert(not module.file_truncate(file({}, "/write")))
assert(not module.sb_mount(nil, mount("/tree"), nil, 0))
assert(not module.move_mount(mount("/outside"), mount("/tree")))
assert(module.sb_mount(nil, mount("/outside"), nil, 0))
assert(not module.sb_mount(nil, mount("/outer", "fuse"), nil, 0))
assert(module.path_unlink(mount("/", "fuse"), dentry("/outer/mounted/data")))
assert(module.sb_mount("/unrelated", mount("/outside"), nil, 4096))
assert(not module.path_rename(root_mount, dentry("/outside", "reg"), root_mount, dentry("/write"), 0))
assert(not module.path_rename(root_mount, dentry("/write", "reg"), root_mount, dentry("/outside"), 2))
local outside_file, filtered_dir = dentry("/outside", "reg"), dentry("/glob/sub", "dir")
assert(module.path_rename(root_mount, outside_file, root_mount, dentry("/delete"), 0))
assert(not module.path_rename(root_mount, outside_file, root_mount, dentry("/delete", "reg"), 0))
assert(module.path_rename(root_mount, outside_file, root_mount, dentry("/glob/sub/other.log", "reg"), 2))
assert(not module.path_rename(root_mount, filtered_dir, root_mount, outside_file, 2))
assert(not module.path_rename(root_mount, outside_file, root_mount, filtered_dir, 2))
assert(events[#events] == "policy_deny")
assert(not module.path_link(dentry("/outside", "reg"), root_mount, dentry("/tree/incoming")))
current = admin
assert(module.path_link(dentry("/outside", "reg"), root_mount, dentry("/tree/incoming")))
current = root
assert(not module.file_permission(file({}, "/tree/incoming"), 2))
assert(module.file_permission(file({}, "/outside"), 2))
assert(not module.file_permission(file({}, "/later/dir/secret"), 4))
assert(module.file_permission(file({}, "/later/dir/other"), 4))
for _, fixture in ipairs({
  {"/glob/", "深.txt", "ab.txt", true}, {"/like/", "literal*?", "skipliteral*?", true},
  {"/simple/", "pre.txt", "pre.log", false}, {"/chain/", "aabab", "aab", false},
}) do
  assert(not module.file_permission(file({}, fixture[1] .. fixture[2]), 4))
  assert(module.file_permission(file({}, fixture[1] .. fixture[3]), 4))
  assert(module.file_permission(file({}, fixture[1] .. "sub/" .. fixture[2]), 4) == not fixture[4])
end
objects["/proc/self/fd/99"] = file({}, "/glob/深.txt")
command("match/7/99")
objects["/proc/self/fd/99"] = file({}, "/glob/ignored")
local _, mismatch = module.file_open(file({}, "/control/match/7/99"))
assert(mismatch == errno.ESRCH)
local thread = task_methods({ group_leader = function() return root end })
current = thread
assert(module.bprm_check_security({ file = function() return file({}, "/executable") end }))
assert(thread.member == true)
function thread:group_leader() return self end
current = root
local child = task_methods({ })
function child:group_leader() return self end
function child:is_descendant() return false end
assert(module.task_alloc(child))
assert(child.member == true)
-- Reparenting and root exit do not remove the inherited object property.
tasks[42] = nil
current = child
assert(module.file_permission(file(exact, "/exact"), 4) == false)
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
-- A second VM retains task membership and evaluates the same path policy.
module = load_module()
assert(module.file_permission(file(exact, "/exact"), 4) == false)
local fault = { inode = function() error("evaluation failed") end }
assert(module.file_permission(fault, 4) == false)
assert(events[#events] == "evaluation_error")
assert(module.file_permission({ inode = function() return nil end }, 4) == false)
assert(module.bprm_check_security({ file = function() return file(exact, "/exact") end }) == false)
assert(module.file_permission(file(exact, "/exact"), 4) == false)
assert(events[#events] == "policy_deny")
local vma = { file = function() return file(exact, "/exact") end, mapping = function() return false end }
assert(module.file_mprotect(vma, 1, 1) == false)
current = admin
assert(module.file_permission(file(exact, "/exact"), 4) == true)
assert(module.file_permission(fault, 4) == true)
if arg[2] then
  local generated = assert(loadfile(arg[2]))()
  current = child
  assert(not generated.file_permission(file({}, "/work/prefix.txt"), 2))
  assert(not generated.file_permission(file({}, "/work/pre/sub/file.txt"), 2))
  assert(generated.file_permission(file({}, "/work/sub/prefix.txt"), 2))
  assert(not generated.path_mknod(ram, dentry('/mounted/quoted"name/prefix.txt')))
  assert(generated.path_mknod(ram, dentry('/mounted/quoted"name/prefix.log')))
end
print("Lua-LSM template: runtime paths, hard-link boundary, independent operations, exclusions, inheritance, VM reload and error policy passed")
