// The cwd must be readable to launch. Its fixture subdirectories need separate grants.
permit(principal, action == SecCore::Action::"file.read", resource)
when {
    resource.path == "/usr" || resource.path like "/usr/*" ||
    resource.path == "/bin" || resource.path like "/bin/*" ||
    resource.path == "/lib" || resource.path like "/lib/*" ||
    resource.path == "/lib64" || resource.path like "/lib64/*" ||
    resource.path == "/etc/ld.so.cache"
};
permit(principal, action == SecCore::Action::"file.read", resource)
when { resource.path == "/run/anolisa-task-policy" || resource.path like "/run/anolisa-task-policy/*" }
unless {
    resource.path == "/run/anolisa-task-policy/config" || resource.path like "/run/anolisa-task-policy/config/*" ||
    resource.path == "/run/anolisa-task-policy/dist" || resource.path like "/run/anolisa-task-policy/dist/*" ||
    resource.path == "/run/anolisa-task-policy/home" || resource.path like "/run/anolisa-task-policy/home/*" ||
    resource.path == "/run/anolisa-task-policy/out" || resource.path like "/run/anolisa-task-policy/out/*" ||
    resource.path == "/run/anolisa-task-policy/project" || resource.path like "/run/anolisa-task-policy/project/*" ||
    resource.path == "/run/anolisa-task-policy/releases" || resource.path like "/run/anolisa-task-policy/releases/*" ||
    resource.path == "/run/anolisa-task-policy/scripts" || resource.path like "/run/anolisa-task-policy/scripts/*"
};
permit(principal, action == SecCore::Action::"process.spawn", resource)
when { resource.program == "/bin/bash" || resource.program == "/usr/bin/bash" };
