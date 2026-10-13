# Tokenless Statistics Recording

[中文版](tokenless-statistics_zh.md)

The server queries a configured Tokenless API with a backend-only credential. It saves
bounded, atomic results in the existing GenAI store to reuse lifecycle and retention policies.
Original source fields remain unchanged; local session association requires a unique captured tool-call ID. Missing or ambiguous matches stay null. Endpoint, source ID and original timestamp
deduplicate refreshes without introducing a recovery protocol. Metadata refresh preserves cached
content. Failures are explicit; saved statistics remain readable when the source is offline.
