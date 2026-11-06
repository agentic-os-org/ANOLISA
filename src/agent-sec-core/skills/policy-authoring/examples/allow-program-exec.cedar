// Allow exactly one program in one working directory to start; refuse all other execution.
// A permit alone cannot narrow an existing broad permit; the complement preserves exclusivity when combined.
permit(principal, action == SecCore::Action::"file.exec", resource is SecCore::File)
when { resource.path == "/usr/bin/git" && context.input.cwd == "/work" };

forbid(principal, action == SecCore::Action::"file.exec", resource is SecCore::File)
unless { resource.path == "/usr/bin/git" && context.input.cwd == "/work" };
