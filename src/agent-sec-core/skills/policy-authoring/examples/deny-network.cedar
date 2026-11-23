// Refuse all network use. The schema has no host, port or protocol dimension, so a
// host-scoped intent cannot be expressed here and must be reported as unresolved instead.
forbid(principal, action == SecCore::Action::"network.use", resource == SecCore::Network::"all");
