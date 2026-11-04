// Allow content reads of the project directory and its whole subtree.
// The second clause is required: "like /dir/*" does not match "/dir" itself.
permit(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
when { resource.path == "/work/project" || resource.path like "/work/project/*" };
