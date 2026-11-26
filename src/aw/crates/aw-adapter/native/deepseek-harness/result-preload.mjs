/** Gate the rc.5 tools registry after final content projection and before publication. */
import { installVerifiedPatch } from "../shared/js-runtime/module-patch.mjs";

const slot = '[Symbol.for("aw.deepseek.resultGate")]';
const prepare = "const created = this.createExecution(input);";
const finish = "this.cancellationResult(exec, postResult) : postResult)";
const publish = "this.notifyResult(exec, finalResult);\n\t\treturn finalResult;";
installVerifiedPatch({
  target: "/node_modules/@deepseek-ai/dsh-tools/lib/index.js",
  digest: "1e53932e7c4213ea24a7d87f06960b7a7702cb21f8a5741c97242b4a470a442a",
  patches: [
    { anchor: "var ToolRuntime = class extends Service {",
      replacement: `var ToolRuntime = class extends Service {
        ${slot} = { current: null, executions: new WeakMap() };` },
    // Pin the owner across scheduler stages, including plugin disposal during execution.
    { anchor: prepare, replacement: `${prepare}
      const slot = this${slot};
      if (slot.current) slot.executions.set(created.exec, slot.current);` },
    { anchor: finish, replacement: finish.slice(0, -1) + ", true)" },
    { anchor: "finishScheduledExecution(exec, result) {",
      replacement: "finishScheduledExecution(exec, result, afterPost = false) {" },
    { anchor: publish, replacement: `const executions = this${slot}.executions;
      const gate = executions.get(exec);
      if (!gate) { ${publish} }
      executions.delete(exec);
      const publishChecked = (checked) => {
        const result = checked === finalResult ? finalResult : this.materializeFinalResult(checked);
        this.notifyResult(exec, result);
        return result;
      };
      if (!afterPost) return publishChecked(gate.skipped(exec, finalResult));
      return gate.after(exec, finalResult).then(checked => publishChecked(
        !checked.isError && this.callerCancelled(exec) ? this.cancellationResult(exec, checked) : checked
      ));` },
  ],
});
