// Allow one write target only: this file, and only the "write" operation.
// The complement negates the full path and operation condition, refusing all other write operations.
permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/work/report" && context.input.operation == SecCore::WriteOperation::"write" };

forbid(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
unless { resource.path == "/work/report" && context.input.operation == SecCore::WriteOperation::"write" };
