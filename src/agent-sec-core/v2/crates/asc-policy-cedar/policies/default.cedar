// Restrictive fallback for a Scope that has no explicit policy configured.
//
// This policy grants read access to the standard system paths, grants read-write
// access to /tmp and /dev/null, leaves process identity to Scope/Binding, and
// blocks all network access: no statement permits network.use, so neither this
// policy nor a provider can reach the network implicitly.
//
// The working directory is deliberately absent. The schema carries no workdir
// fact and no statement can discover one at evaluation time, so an author adds
// the real path when binding this policy; a placeholder here would silently
// grant whatever path happens to carry that name on the host.
//

// System directories are readable, matching a container baseline.
permit(
    principal,
    action == SecCore::Action::"file.read",
    resource
)
when {
    resource.path == "/bin" || resource.path like "/bin/*" ||
    resource.path == "/usr" || resource.path like "/usr/*" ||
    resource.path == "/lib" || resource.path like "/lib/*" ||
    resource.path == "/lib64" || resource.path like "/lib64/*" ||
    resource.path == "/etc" || resource.path like "/etc/*" ||
    resource.path == "/var/log" || resource.path like "/var/log/*" ||
    resource.path == "/proc" || resource.path like "/proc/*" ||
    resource.path == "/dev/urandom" || resource.path == "/dev/null" ||
    resource.path == "/tmp" || resource.path like "/tmp/*"
    // The workdir stays unlisted on purpose; add the real path when authoring.
    // || resource.path == "/workspace" || resource.path like "/workspace/*"
};

// Credentials, host keys and other process memory stay unreadable even though
// their parent directory above is readable.
forbid(
    principal,
    action == SecCore::Action::"file.read",
    resource
)
when {
    resource.path == "/etc/shadow" || resource.path == "/etc/shadow-" ||
    resource.path == "/etc/gshadow" || resource.path == "/etc/gshadow-" ||
    resource.path == "/etc/sudoers" || resource.path like "/etc/sudoers.d/*" ||
    resource.path == "/etc/security/opasswd" ||
    resource.path == "/etc/krb5.keytab" ||
    resource.path == "/etc/ssl/private" || resource.path like "/etc/ssl/private/*" ||
    resource.path == "/etc/pki/tls/private" ||
    resource.path like "/etc/pki/tls/private/*" ||
    // Certbot keeps numbered archive keys and live aliases beside public certificates.
    resource.path like "/etc/letsencrypt/*/privkey*.pem" ||
    resource.path like "/etc/ssh/ssh_host_*" ||
    // Private keys stay unreadable even if an author grants a home directory.
    resource.path like "/*/.ssh/*" ||
    resource.path == "/etc/crypttab" ||
    resource.path like "/proc/*/mem" || resource.path like "/proc/*/environ" ||
    // procfs exposes memory and magic links that reach outside the permitted
    // paths: kcore mirrors kernel memory, fd/cwd/exe point anywhere on the
    // host, root/* escapes into another mount namespace, and maps leak the
    // address layout of every process.
    resource.path == "/proc/kcore" ||
    // kallsyms and auxv disclose the same address-layout surface as maps.
    resource.path == "/proc/kallsyms" ||
    resource.path like "/proc/*/auxv" ||
    resource.path like "/proc/*/fd/*" || resource.path like "/proc/*/root/*" ||
    resource.path like "/proc/*/cwd/*" || resource.path like "/proc/*/exe" ||
    resource.path like "/proc/*/maps" || resource.path like "/proc/*/smaps" ||
    // The bare fd/root/cwd directories and map_files entries stay unreadable
    // too: listing them discloses the fd set, the other namespace's layout and
    // the mapped address ranges.
    resource.path like "/proc/*/fd" || resource.path like "/proc/*/root" ||
    resource.path like "/proc/*/cwd" ||
    resource.path like "/proc/*/map_files" ||
    resource.path like "/proc/*/map_files/*"
};

// Only ephemeral paths are writable; the workdir stays unlisted like the read
// grant above.
permit(
    principal,
    action == SecCore::Action::"file.write",
    resource
)
when {
    resource.path == "/dev/null" ||
    resource.path == "/tmp" || resource.path like "/tmp/*"
    // || resource.path == "/workspace" || resource.path like "/workspace/*"
};

// System trees may run, covering workload programs and the dynamic linker
// under /lib and /lib64; data files inside these trees carry no execute bit.
permit(
    principal,
    action == SecCore::Action::"file.exec",
    resource
)
when {
    resource.path like "/bin/*" ||
    resource.path like "/usr/*" ||
    resource.path like "/lib/*" ||
    resource.path like "/lib64/*"
    // || resource.path like "/workspace/*"
};

// No statement permits SecCore::Action::"network.use", so outbound network
// access stays closed until an explicit policy opens it.
