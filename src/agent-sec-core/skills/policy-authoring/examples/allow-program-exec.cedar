// Allow exactly one program in one working directory to start; refuse all other execution.
// A permit alone would leave every other program reachable, so the complement forbid is required.
permit(principal, action == SecCore::Action::"file.exec", resource is SecCore::File)
when { resource.path == "/usr/bin/git" && context.input.cwd == "/work" };

forbid(principal, action == SecCore::Action::"file.exec", resource is SecCore::File)
unless { resource.path == "/usr/bin/git" && context.input.cwd == "/work" };
