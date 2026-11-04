// Keep every write permission intact but refuse the delete operation everywhere.
// The operation condition is required: without it this would forbid all writes.
forbid(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { context.input.operation == SecCore::WriteOperation::"delete" };
