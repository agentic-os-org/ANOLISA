// Ask for the review root first; this example assumes the user confirmed /work/project and its whole subtree.
// Reads are authorized by path, without distinguishing code from sensitive files inside the directory.
permit(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
when { resource.path == "/work/project" || resource.path like "/work/project/*" };

// Refuse reads outside the directory even when combined with an existing broad read permit.
forbid(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
unless { resource.path == "/work/project" || resource.path like "/work/project/*" };

forbid(principal, action == SecCore::Action::"file.write", resource is SecCore::File);

forbid(principal, action == SecCore::Action::"file.exec", resource is SecCore::File);

forbid(principal, action == SecCore::Action::"network.use", resource == SecCore::Network::"all");
