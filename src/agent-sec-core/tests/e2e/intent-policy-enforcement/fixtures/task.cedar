// CANDIDATE: not approved; not effective.
// unresolved: []
// HUMAN GATE: these permits stop for confirmation before taking effect. A path listed here is also granted by another permit of the same action, so the gate is friction and not a wider grant; a network.use gate is the whole-network grant itself, so nothing is allowed without confirming it:
//   #4 @id("R2") network.use -- 确实需要联网时，每条命令都要先在审批卡上问我
//   #6 @id("R4") /run/anolisa-task-policy/config -- config/ 下是我手工维护的构建配置，每次写入前都要先问我
//   #7 @id("R5") /run/anolisa-task-policy/dist -- dist/ 是正式发布区，每次写入前都要先问我
// COVERAGE: who can carry each requirement, from statement shape only:
//   permit -> sandbox + PDP (a permit testing context -> sandbox only; network.use -> PDP);
//   forbid on file.read or file.write -> os-harness (any other forbid -> none).
//   A Scope still has to apply it; a requirement without statements is not listed.
//   base  #1 #2  sandbox, PDP
//   R1    #3     os-harness
//   R2    #4     PDP
//   R3    #5     os-harness
//   R4    #6     sandbox, PDP
//   R5    #7     sandbox, PDP
//   goal  #8     sandbox, PDP
// ============================================================
// 基础授权（程序生成，不来自 task）：读取上限与 shell 启动
// 不是安全要求，只让命令能够启动；读取限制与写入范围由下面的要求给出。
// ============================================================

@id("base")
permit(
  principal,
  action == SecCore::Action::"file.read",
  resource is SecCore::File
)
when {
  resource.path == "/" || resource.path like "/*"
};

@id("base.2")
permit(
  principal,
  action == SecCore::Action::"process.spawn",
  resource is SecCore::Process
)
when {
  resource.program == "bash" || resource.program == "/bin/bash" ||
  resource.program == "/usr/bin/bash"
};

// ============================================================
// R1：保护 releases/previous.zip 不被删除
// 回滚用的既有包不得删除，其余写入不受影响
// ============================================================
@id("R1")
forbid(
    principal,
    action == SecCore::Action::"file.write",
    resource is SecCore::File
)
when {
    resource.path == "/run/anolisa-task-policy/releases/previous.zip" &&
    context.input.operation == SecCore::WriteOperation::"delete"
};

// ============================================================
// R2：不允许联网；确需联网时每条命令先审批
// 整网 permit 加 @advice，未确认不执行
// ============================================================
@id("R2")
@advice("{\"verdict\":\"escalate\",\"message\":\"确实需要联网时，每条命令都要先在审批卡上问我\"}")
permit(
    principal,
    action == SecCore::Action::"network.use",
    resource == SecCore::Network::"all"
);

// ============================================================
// R3：不允许读 project/deploy.env
// 凭据文件禁止读取
// ============================================================
@id("R3")
forbid(
    principal,
    action == SecCore::Action::"file.read",
    resource is SecCore::File
)
when {
    resource.path == "/run/anolisa-task-policy/project/deploy.env"
};

// ============================================================
// R4：config/ 允许写，但每次都要先问我
// 写入范围已在 goal 中授予，此处仅加确认
// ============================================================
@id("R4")
@advice("{\"verdict\":\"escalate\",\"message\":\"config/ 下是我手工维护的构建配置，每次写入前都要先问我\"}")
permit(
    principal,
    action == SecCore::Action::"file.write",
    resource is SecCore::File
)
when {
    resource.path == "/run/anolisa-task-policy/config" ||
    resource.path like "/run/anolisa-task-policy/config/*"
};

// ============================================================
// R5：dist/ 允许写，但每次都要先问我
// 写入范围已在 goal 中授予，此处仅加确认
// ============================================================
@id("R5")
@advice("{\"verdict\":\"escalate\",\"message\":\"dist/ 是正式发布区，每次写入前都要先问我\"}")
permit(
    principal,
    action == SecCore::Action::"file.write",
    resource is SecCore::File
)
when {
    resource.path == "/run/anolisa-task-policy/dist" ||
    resource.path like "/run/anolisa-task-policy/dist/*"
};

// ============================================================
// goal：只允许写 releases/、out/、config/ 和 dist/
// 按写明的四个目录授权，不扩大范围
// ============================================================
@id("goal")
permit(
    principal,
    action == SecCore::Action::"file.write",
    resource is SecCore::File
)
when {
    resource.path == "/run/anolisa-task-policy/releases" ||
    resource.path like "/run/anolisa-task-policy/releases/*" ||
    resource.path == "/run/anolisa-task-policy/out" ||
    resource.path like "/run/anolisa-task-policy/out/*" ||
    resource.path == "/run/anolisa-task-policy/config" ||
    resource.path like "/run/anolisa-task-policy/config/*" ||
    resource.path == "/run/anolisa-task-policy/dist" ||
    resource.path like "/run/anolisa-task-policy/dist/*"
};
