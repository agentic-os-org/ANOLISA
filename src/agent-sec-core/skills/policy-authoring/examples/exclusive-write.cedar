// Allow one write target only: this file, and only the "write" operation.
// The complement forbid keeps the same operation condition, so create/delete/rename/setattr
// elsewhere stay unaffected by this rule.
permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/work/report" && context.input.operation == SecCore::WriteOperation::"write" };

forbid(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { context.input.operation == SecCore::WriteOperation::"write" && resource.path != "/work/report" };
