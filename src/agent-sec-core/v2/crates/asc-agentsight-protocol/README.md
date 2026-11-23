# AgentSight protocol types

`asc-agentsight-protocol` defines the file-protection wire types supported by
AgentSecCore's AgentSight Translator and Deployment Client. It contains the
version-1 payload, its media type and serde definitions. The Translator constructs
these types; the Client validates and sends them through the HTTP API.

The crate is maintained in the SecCore workspace and depends only on `serde`.
Protocol changes require an explicit supported-version decision and compatibility
validation against the target release. Fixed JSON fixtures in the Translator and
Client tests check the serialized contract.

See the [Cedar mapping contract](../../../docs/design/AGENTSIGHT_CEDAR_TRANSLATION.md)
for supported expressions and enforcement limits.
