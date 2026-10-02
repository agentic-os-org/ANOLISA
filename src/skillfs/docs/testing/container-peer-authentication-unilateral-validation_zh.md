# 容器 Peer 认证单方面验证计划

[English](container-peer-authentication-unilateral-validation.md)

这是 [issue #2439](https://github.com/alibaba/anolisa/issues/2439) 的 SkillFS
侧测试执行计划。它可以在真实 Linux 或 Kubernetes 环境验证 container HMAC
合同，不需要等待独立发布的 agent-sec-core image。

独立的 Python 标准库 probe 位于
[`scripts/container-peer-auth-probe.py`](../../scripts/container-peer-auth-probe.py)，
它只扮演 control client 或 notify server。通过本计划可以证明 SkillFS 实现和
跨语言 wire contract，但不能证明 agent-sec-core 配置、reconcile、activation
decision 或发布就绪；这些仍需要双方联调和 ACK 证据。

## 1. 固定测试输入

修改环境前，在仓库外创建 evidence 目录并记录以下信息：

```bash
export C7_EVIDENCE=/var/tmp/skillfs-c7-evidence
install -d -m 0700 "$C7_EVIDENCE"
git rev-parse HEAD | tee "$C7_EVIDENCE/git-commit.txt"
rustc --version | tee "$C7_EVIDENCE/rustc.txt"
python3 --version | tee "$C7_EVIDENCE/python.txt"
uname -a | tee "$C7_EVIDENCE/uname.txt"
```

Kubernetes 环境还需要记录不可变的集群和 image 输入：

```bash
kubectl version -o yaml >"$C7_EVIDENCE/kubernetes-version.yaml"
kubectl get nodes -o wide >"$C7_EVIDENCE/nodes.txt"
kubectl get nodes -o jsonpath='{range .items[*]}{.metadata.name}{"\t"}{.status.nodeInfo.containerRuntimeVersion}{"\n"}{end}' \
  >"$C7_EVIDENCE/container-runtime.txt"
kubectl -n "$C7_NAMESPACE" get pod "$C7_POD" -o yaml \
  >"$C7_EVIDENCE/pod.yaml"
kubectl -n "$C7_NAMESPACE" get pod "$C7_POD" \
  -o jsonpath='{range .status.containerStatuses[*]}{.name}{"\t"}{.imageID}{"\n"}{end}' \
  >"$C7_EVIDENCE/image-ids.txt"
```

不要把 Secret object、raw secret、proof 或可重用 credential 保存到 evidence
目录。

## 2. 必需拓扑

使用三个 container，`shareProcessNamespace` 必须省略或设置为 `false`：

| Container | Effective UID | Source | FUSE view | Runtime socket | Auth file |
| --- | ---: | --- | --- | --- | --- |
| SkillFS | 本轮使用 0 | read-write | mount 创建方 | read-write | read-only |
| trusted probe | 本轮使用 0 | 相同绝对路径上的同一物理 source | 可选 | read-write | read-only |
| workload / attacker | 普通 I/O 使用不同 UID；socket 负向测试再用 UID 0 重复 | 不挂载 | read-write，传播获得 | 默认不挂载；负向测试只读挂载 | 不挂载 |

以下内容分别使用独立 volume：

- physical source；
- propagated FUSE view；
- private runtime socket；
- staged authentication file。

Kubernetes Secret projection 的文件入口是 symlink，而生产 loader 使用
`O_NOFOLLOW` 打开最终路径。因此不能把 projected Secret 路径直接传给 SkillFS。
应由 init container 将 projected key 复制到可信 `emptyDir`，设置为 `0400` 或
`0600`，并且只挂载给 SkillFS 和 trusted probe。这是验收要求，不是放宽
no-follow 规则。

首版 profile 要求 SkillFS 与 sec-core 在 authenticated notify 场景使用相同的
effective UID，因为 SkillFS 会用自己的 effective UID 校验 sec-core 创建的 endpoint
及其父目录。每个 auth file 也必须由读取它的进程 effective UID 所有。未来支持
不同 UID 时，除了分别 stage 包含相同 raw bytes 的私有 key 副本，还必须明确引入
新的 endpoint ownership policy。

将 probe script mount 或复制到 trusted probe container。Evidence 可以写入挂载在
`$C7_EVIDENCE` 的私有目录；否则先写入 probe container，再逐个使用 `kubectl cp`
取回。Probe 和 evidence volume 都不能出现在 workload container 中。

## 3. 准备 fixture

可信 container 中统一使用以下路径：

```bash
export C7_SOURCE=/var/lib/skillfs/source
export C7_MOUNT=/var/lib/skillfs/shared/mount
export C7_RUNTIME=/run/anolisa
export C7_SECRET=/run/anolisa/auth/skillfs.key
export C7_CONTROL=/run/anolisa/skillfs/control.sock
export C7_NOTIFY=/run/anolisa/probe/notify.sock
export C7_PROBE=/workspace/src/skillfs/scripts/container-peer-auth-probe.py
```

创建 Kubernetes Secret 前生成一份 raw random key：

```bash
umask 077
head -c 32 /dev/urandom >skillfs.key
test "$(wc -c <skillfs.key)" -eq 32
kubectl -n "$C7_NAMESPACE" create secret generic skillfs-c7-auth \
  --from-file=skillfs.key=skillfs.key
```

准备带可见 snapshot 的 Flat-layout Skill。Live directory 也必须包含
`SKILL.md`，这样 `skill.resolveLiveSource` 才能完成验证：

```bash
install -d -m 0755 \
  "$C7_SOURCE/weather/.skill-meta/versions/v000001.snapshot"
printf '%s\n' '---' 'name: weather' 'description: C7 validation' '---' \
  >"$C7_SOURCE/weather/SKILL.md"
cp "$C7_SOURCE/weather/SKILL.md" \
  "$C7_SOURCE/weather/.skill-meta/versions/v000001.snapshot/SKILL.md"
printf '%s\n' '{"schemaVersion":1,"target":".skill-meta/versions/v000001.snapshot"}' \
  >"$C7_SOURCE/weather/.skill-meta/activation.json"
```

使用显式 container profile，以 foreground 方式启动 SkillFS：

```bash
skillfs mount "$C7_SOURCE" "$C7_MOUNT" \
  --foreground --allow-other \
  --security --activation-mode file \
  --notify-socket "$C7_NOTIFY" \
  --notify-auth-key-file "$C7_SECRET" \
  --control-socket "$C7_CONTROL" \
  --trusted-peer-key-file "$C7_SECRET"
```

继续测试前必须满足：

- SkillFS process 保持运行；
- `/proc/self/mountinfo` 中存在 mount；
- workload 的 propagated view 可以读取 `skills/weather/SKILL.md`；
- control socket 位于私有 runtime directory，mode 为 `0600`；
- notify listener 使用 owner 匹配、不含 group/other bits 的 socket，并直接位于
  owner 匹配且不含 group/other bits 的目录下，目录推荐使用 `0700`。

## 4. Control 正向测试

在 trusted probe container 执行：

```bash
python3 "$C7_PROBE" control \
  --socket "$C7_CONTROL" --secret "$C7_SECRET" \
  --request '{"schemaVersion":"1","method":"ping"}' \
  | tee "$C7_EVIDENCE/control-ping.json"

python3 "$C7_PROBE" control \
  --socket "$C7_CONTROL" --secret "$C7_SECRET" \
  --request '{"schemaVersion":"1","method":"status"}' \
  | tee "$C7_EVIDENCE/control-status.json"

python3 "$C7_PROBE" control \
  --socket "$C7_CONTROL" --secret "$C7_SECRET" \
  --request "{\"schemaVersion\":\"1\",\"method\":\"skill.resolveLiveSource\",\"canonicalSkillDir\":\"$C7_SOURCE/weather\"}" \
  | tee "$C7_EVIDENCE/control-resolve.json"
```

预期结果：

- 每个 process 完成 mutual authentication 后以零退出；
- `ping` 返回 `ok=true` 和 `pong=true`；
- `status` 保持现有 schema v1 business response；
- resolve 返回 `managed=true`、`skillId=weather`、
  `liveSkillDir=$C7_SOURCE/weather` 和 `transport=shared_path`；
- 每条连接输出的 nonce 都不同。

## 5. Control 负向测试

### 5.1 Plain 和错误 key peer

在 attacker container 中故意只挂 runtime socket，不挂真实 auth file。先使用普通
workload UID，再使用与 SkillFS 相同的 UID 0 重复执行：

```bash
python3 "$C7_PROBE" plain --socket "$C7_CONTROL"
umask 077
head -c 32 /dev/urandom >/tmp/wrong-skillfs.key
if python3 "$C7_PROBE" control \
  --socket "$C7_CONTROL" --secret /tmp/wrong-skillfs.key \
  --request '{"schemaVersion":"1","method":"ping"}'; then
  echo 'FAIL: wrong-key peer was accepted' >&2
  exit 1
else
  echo 'PASS: wrong-key peer was rejected'
fi
```

Plain probe 必须输出 `PASS`。错误 key command 必须失败，并且不能收到 business
response。SkillFS 必须保持 mounted，随后 trusted `ping` 仍需成功。

### 5.2 Replay、业务篡改和缓慢发送的不完整 frame

```bash
python3 "$C7_PROBE" replay \
  --socket "$C7_CONTROL" --secret "$C7_SECRET" --timeout 6
python3 "$C7_PROBE" tamper \
  --socket "$C7_CONTROL" --secret "$C7_SECRET" --timeout 6
python3 "$C7_PROBE" slow \
  --socket "$C7_CONTROL" --interval 1 --bound 7
python3 "$C7_PROBE" control \
  --socket "$C7_CONTROL" --secret "$C7_SECRET" \
  --request '{"schemaVersion":"1","method":"ping"}'
```

Replay 必须观察到新 nonce，并且不能收到 `auth.ok`。Tamper probe 必须完成有效握手，
但在修改 raw request 且未提供匹配 tag 后不能收到 response。Slow sender 必须在七秒
内被断开。最后的 trusted request 必须通过，证明单请求 listener 已恢复。

### 5.3 Secret 和配置启动门禁

每一行都启动新的 SkillFS process，并确认它在留下 FUSE mount 或 socket 前以非零
退出：

| Case | Setup | 预期 diagnostic |
| --- | --- | --- |
| missing | 配置的文件不存在 | cannot open authentication key |
| projected symlink | 直接使用 Kubernetes Secret entry | no-follow open failure |
| FIFO | mode `0600` 且没有 writer | 不阻塞并拒绝非普通文件 |
| short | 31 raw bytes | must contain 32–4096 bytes |
| oversized | 4097 raw bytes | must contain 32–4096 bytes |
| permissive | mode `0640` 或 `0644` | 拒绝 group 或 other permission |
| wrong owner | owner 与 effective UID 不同 | 拒绝非 effective-user owner |
| half-hardened | HMAC control 加 notify socket，但不提供 notify key | 要求 notify auth key |
| mixed modes | 同时配置 trusted peer executable 和 key | mutually exclusive |

每次失败后都保存 stderr，通过 `findmnt` 确认没有 residual mount，并确认 control、
notify socket 均未残留。

## 6. Notify 正向测试

触发 mutation 前，在 trusted probe container 启动独立 notify server：

```bash
python3 "$C7_PROBE" notify-server \
  --socket "$C7_NOTIFY" --secret "$C7_SECRET" \
  --output "$C7_EVIDENCE/notify-request.json" \
  >"$C7_EVIDENCE/notify-probe.log" 2>&1 &
C7_NOTIFY_PID=$!
```

等待 `READY`，然后通过 workload 可见的 FUSE view 写入：

```bash
grep -q '^READY:' "$C7_EVIDENCE/notify-probe.log"
date -u +%FT%TZ >"$C7_MOUNT/skills/weather/validation.txt"
wait "$C7_NOTIFY_PID"
python3 -m json.tool "$C7_EVIDENCE/notify-request.json"
```

预期结果：

- probe 输出 `PASS: authenticated notify v2 recorded`；
- request method 是 `skill_ledger.skillfs_notify_change`；
- `params.schemaVersion` 是 `2`；
- `params.canonicalSkillDir` 是 `$C7_SOURCE/weather`；
- `params.skillId` 是 `weather`，changed path 使用相对路径；
- 即使后续 notification delivery 失败，普通 FUSE I/O 仍能完成。

Startup reconcile 可能产生第一条 notify。若发生，单独保存该证据，重新启动一次性
probe，再执行 workload mutation。

## 7. 错误 notify server proof

让 probe 故意返回错误 server proof，再触发一次 FUSE mutation：

```bash
python3 "$C7_PROBE" notify-server \
  --socket "$C7_NOTIFY" --secret "$C7_SECRET" \
  --output "$C7_EVIDENCE/notify-must-not-exist.json" \
  --wrong-server-proof \
  >"$C7_EVIDENCE/notify-wrong-server.log" 2>&1 &
C7_BAD_NOTIFY_PID=$!
date -u +%FT%TZ >"$C7_MOUNT/skills/weather/wrong-server-proof.txt"
wait "$C7_BAD_NOTIFY_PID"
test ! -e "$C7_EVIDENCE/notify-must-not-exist.json"
```

Probe 必须报告错误 proof 后没有 business frame。SkillFS 必须记录 authentication
failure、继续服务 FUSE I/O，并保持 activation mapping 不变。

## 8. Namespace 和 volume 隔离

在 workload container 中记录并断言：

```bash
test ! -e "$C7_SOURCE"
test ! -e "$C7_SECRET"
test ! -e "$C7_CONTROL"
test ! -e "$C7_NOTIFY"
cat "$C7_MOUNT/skills/weather/SKILL.md" >/dev/null
```

记录每个 container 的 PID 和 mount namespace identifier，SkillFS 与 trusted probe
的值必须不同。不得为了让 executable authentication 工作而开启 shared PID
namespace。

对于故意设置的 attacker case，只挂 runtime volume，然后重复 plain 和错误 key
测试。仅能看到 socket 不能获得 authority。

## 9. Restart 和 shutdown

1. 记录 SkillFS container restart count 和一次成功的 nonce。
2. 终止 SkillFS sidecar PID 1，等待 kubelet 完成重启。
3. 再次要求 mount probe 通过，并发送 authenticated `ping`，nonce 必须是新值。
4. 重启 trusted notify probe，新的 mutation 必须重新认证并成功 delivery。
5. 启动 slow control probe 后终止 SkillFS，shutdown 和 unmount 必须在 handshake
   bound 加正常 container termination grace period 内完成。
6. 正常删除 Pod，等待退出后在同一 node 重新创建。启动不得报告 residual FUSE
   mount 或 stale socket。

保存重启前后的 restart count、Pod event、SkillFS log、probe output 和 mount probe
output。

## 10. Credential 泄露检查

收集 SkillFS 和 probe log，然后在 trusted container 内比较，但不要打印
credential encoding：

```bash
python3 - "$C7_SECRET" "$C7_EVIDENCE/combined.log" <<'PY'
import base64
import pathlib
import sys

secret = pathlib.Path(sys.argv[1]).read_bytes()
logs = pathlib.Path(sys.argv[2]).read_bytes()
markers = (secret, secret.hex().encode(), base64.b64encode(secret))
if any(marker in logs for marker in markers):
    raise SystemExit("FAIL: credential material appears in logs")
print("PASS: raw, hex, and Base64 credential material absent from logs")
PY
```

同时检查 JSON response 和 protocol-event output。它们可以标识 authentication
mode，但不能包含 secret、proof 或可重用 credential。

## 11. 验收记录

每项标记为 `PASS`、`FAIL` 或 `NOT RUN`，并填写 evidence 文件名：

| ID | 必需结果 |
| --- | --- |
| C7-U01 | 真实 FUSE mount 和 propagated workload read 通过 |
| C7-U02 | authenticated ping、status 和 shared-path resolve 通过 |
| C7-U03 | 每条连接 nonce 不同，并拒绝 replay 和被修改的业务 frame |
| C7-U04 | 拒绝 plain、错误 key、相同 UID 但无 Secret 的 peer |
| C7-U05 | 缓慢发送的不完整认证有界，listener 可以恢复 |
| C7-U06 | 所有 secret/config 启动门禁在 mount 前失败 |
| C7-U07 | 收到 authenticated notify v2，business schema 保持不变 |
| C7-U08 | 错误 notify server proof 阻止 business request |
| C7-U09 | workload 无法看到 source、Secret 和 private socket |
| C7-U10 | SkillFS 和 probe restart 后使用新 challenge 重新认证 |
| C7-U11 | graceful deletion 不留下 residual FUSE mount 或 socket |
| C7-U12 | log、response 和 event 不泄露 credential material |

任意必需项失败都会阻断单方面验收。只有环境无法提供所需 FUSE、privilege、
propagation 或 namespace feature 时才允许 `NOT RUN`，并且必须记录具体 blocker。

## 12. 结论解释

全部通过后，可以让 sec-core maintainer 根据冻结的
[`container-peer-authentication.md`](../design/container-peer-authentication.md)
实现或 review。但这不足以关闭 #2439。关闭前还需要在独立 container 中完成真实
agent-sec-core resolver、notify、activation、failure、restart 联调，并补充聚焦的
ACK 记录。
