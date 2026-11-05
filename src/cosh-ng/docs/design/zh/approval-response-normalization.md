# 审批响应归一化

Core在`crates/cosh-core/src/core/approval_response.rs`归一化当前审批的原始JSONL帧。
普通工具、sandbox-bypass和Prompt Hook等待复用这一owner。认证、问答和ShellEvidence
交换保留原有共享协议DTO，不全局套用此处严格的审批body。

## 不变量

1. 外层`request_id`是既有关联授权真源。合法且非空的其他string ID在解析body前忽略；
   缺失、空、错类型或重复ID不能授权执行，也不能撤销checkpoint。
2. 只有当前ID的`control_response`和`success` envelope能产生决定。当前malformed帧
   以`Unconfirmed`结束等待，不能被随后同ID的裸deny覆盖。整帧owner只返回`Ignore`、
   `Receipt`、`Interrupted`或`Decision`，wait不再运行更宽松的第二个parser。空行、EOF
   和原有deadline保留生命周期。receipt只接受type及非空string request_id；携执行payload
   或其他malformed receipt为`Unconfirmed`。合法已知非审批消息复用共享DTO，仅当top和
   nested ID均不属于当前审批时忽略；合法interrupt仍取消provider。其他当前ID帧、未知kind
   和malformed帧均为`Unconfirmed`。
3. 私有Serde tagged union声明allow、deny、shell结果、checkpoint结果、checkpoint错误
   五种互斥body；各variant拒绝无关字段，包括值为null的执行payload键。null本身不证明
   已执行，但矛盾形状同样不能证明未执行。
4. 所有审批交付在写Host请求前先持久arm现有checkpoint并置memory pending，包括Prompt
   Hook和ReadOnly/Other的PreToolUse Ask；请求工具kind不能证明异常Host未执行。
   unknown、malformed、错kind执行响应、超时或中断均保留保护，即使进程在读取任何完成
   响应前终止也受保护。mandatory批准策略先取原pending，不能把本次临时exposure误当旧失败。
   只有完整工具及bypass生命周期均由严格裸决定证明未执行时，才分别恢复本次新增的memory
   与checkpoint基线。Prompt/只读裸allow可恢复新保护，本地副作用Allowed仍跨执行保持arm。
   原pending及合法memory=false/checkpoint=true提交前拆分都保留；已批准成功仍沿原最终成功
   及commit清除边界。任何可失败audit或提前退出保留已arm保护。
5. 从原始line直接解码，Serde拒绝已知header、behavior、payload、typed outcome和
   metadata安全字段的重复；授权不使用已经丢失重复键的Value map。
6. Shell的`metadata.approval_id`属于内部审批ID，`metadata.tool_use_id`和既有
   `toolUseID`属于诊断tool标识。保留string/null类型验证，不升级为新授权selector；
   Gateway动态分配的checkpoint ID仍沿用已有canonical校验。

## 输入矩阵

| 输入 | 归一化结果 |
| --- | --- |
| 裸allow；可选message string/null、updatedPermissions array/null、toolUseID string/null | 原tool/profile规则下Allowed |
| 裸deny；可选message string/null | Denied，可证明未执行 |
| Prompt或ReadOnly/Other Ask收到执行型/矛盾响应 | Unconfirmed，与请求kind无关地保留exposure guard |
| 非副作用审批裸allow/deny，且后续bypass均未执行 | 仅恢复本次exposure，保留原memory/checkpoint值 |
| 交付审批后、读取任何完成响应前进程终止 | resume要求fresh approval |
| checkpoint arm/身份/写入/同步失败 | 不交付审批请求，也不执行effect |
| 裸current/foreign receipt | current解除收据deadline，foreign忽略 |
| receipt携执行payload/未知字段或非法ID | Unconfirmed，不能藏匿执行后再用裸deny撤销guard |
| 未支持kind，包括当前ID及result | Unconfirmed |
| 合法无关联user、registry或非interrupt control消息 | 按原共享DTO合同Ignore |
| 已知非审批kind含当前top或nested ID | Unconfirmed；合法interrupt仍为Interrupted |
| allow/deny混result、checkpointResult或checkpointError，包括null/错类型 | Unconfirmed；不能本地重放或撤销checkpoint |
| 审批body未知字段、未知behavior、envelope/body malformed、错误subtype | Unconfirmed |
| 当前malformed帧后紧跟同ID裸deny | 保留首帧Unconfirmed |
| 合法其他request ID后跟当前裸deny | 忽略前帧，正常接受当前拒绝 |
| 仅含exit_code=0的合法legacy Shell结果 | 已确认Shell成功 |
| Shell结果returnDisplay=null | 合法可选显示字段 |
| metadata或exit_code缺失/null | Shell完成未知，保留pending |
| 非null exit_code不是i32范围内JSON整数 | Unconfirmed，保留invalid exit_code诊断 |
| exit_code=0且已提供status=completed、signal缺失/null、任何input_wait均明确interrupted=false | 已确认Shell成功 |
| exit_code=0但已提供status未知/null/错型、非null signal或interrupted未知/true | Unconfirmed，不能用0清掉矛盾证据 |
| 合法非零exit_code | Shell失败；普通诊断status不能授权或清保护 |
| Shell结果混checkpoint payload，或用于非Shell/brokered工具 | Unconfirmed |
| 精确checkpoint profile和HostedSideEffect的checkpoint结果 | 复用已有typed校验，合法created/skipped成功 |
| 同profile/kind的checkpoint错误 | 合法failed/uncertain保留保护 |
| typed payload缺失/null/错型、混payload、ID/outcome/bounds非法或typed未知字段 | Unconfirmed |
| 已知字段重复，包括exit_code先1后0 | Unconfirmed，不用最后一个值授权 |
| Prompt Hook收到Host完成或含执行结果的allow | Unconfirmed，不能放行Prompt |

status、signal和input_wait.interrupted的一致性只约束0退出成功的确认，不重新解释
已确定的非零失败，也不收紧未来普通metadata诊断。status缺失保留legacy仅exitcode
响应兼容；显式null status不能确认成功。

## producer兼容与验证

维护中的Shell allow编码只有裸allow，Shell及Gateway deny编码只有behavior与message，
不会在deny中附带nullable执行payload。Shell完成响应的显示/诊断字段有合法null，
继续支持。Core原来可读的allow兼容字段updatedPermissions/toolUseID保留类型合同。
此处不新增公共control-wire字段、认证/问答schema或依赖。

`core/tests/approval_response_matrix.rs`用原始帧驱动既有等待入口。真实turn/persist
场景在`headless/approval_response_safety_tests.rs`：读取对应Host响应时实际写effect，
随后检查durable pending、没有本地重放、Auto后续写被拦及bypass处理。原deny、成功
clear、receipt、transport、range、typed profile和恢复对照继续保留。只有纯parser
通过不能证明持久审批合同完成。
