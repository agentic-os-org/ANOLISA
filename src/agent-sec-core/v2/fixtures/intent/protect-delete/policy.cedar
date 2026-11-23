permit(
    principal,
    action == SecCore::Action::"file.write",
    resource is SecCore::File
)
when {
    (resource.path == "/work" || resource.path like "/work/*")
    && context.input.operation == SecCore::WriteOperation::"delete"
};
forbid(
    principal,
    action == SecCore::Action::"file.write",
    resource is SecCore::File
)
when {
    resource.path == "/work/config/important.txt"
    && context.input.operation == SecCore::WriteOperation::"delete"
};
forbid(principal, action == SecCore::Action::"file.exec", resource);
forbid(principal, action == SecCore::Action::"network.use", resource);
