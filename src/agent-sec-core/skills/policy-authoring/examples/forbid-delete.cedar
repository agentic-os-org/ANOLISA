// Supplement existing user-confirmed write permissions by refusing deletion; this rule grants no write permission.
// The operation condition is required: without it this would forbid all writes.
forbid(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { context.input.operation == SecCore::WriteOperation::"delete" };
