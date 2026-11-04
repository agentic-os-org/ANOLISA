// Refuse every write operation on one exact file, whatever a broader write permit allows.
// Pair this with a subtree permit; the forbid overrides it for this path only.
forbid(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/work/config/important.txt" };
