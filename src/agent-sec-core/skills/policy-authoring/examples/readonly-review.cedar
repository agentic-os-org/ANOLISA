// Complete read-only review policy: read anything, never write, execute or reach the network.
// Every clause is written in the accepted subset (bare principal, no annotations,
// canonical paths) so the same text passes the official checks unchanged.
permit(principal, action == SecCore::Action::"file.read", resource is SecCore::File);

forbid(principal, action == SecCore::Action::"file.write", resource is SecCore::File);

forbid(principal, action == SecCore::Action::"file.exec", resource is SecCore::File);

forbid(principal, action == SecCore::Action::"network.use", resource == SecCore::Network::"all");
