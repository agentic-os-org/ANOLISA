# Tokenless Statistics Recording

[中文版](tokenless-statistics_zh.md)

The server queries a configured Tokenless API with a backend-only credential. It saves
bounded, atomic results in the existing GenAI store to reuse lifecycle and retention policies.
Original source fields remain unchanged; local session association requires a unique captured tool-call ID. Missing or ambiguous matches stay null. Endpoint, source ID and original timestamp
deduplicate refreshes without introducing a recovery protocol. Metadata refresh preserves cached
content. Failures are explicit; saved statistics remain readable when the source is offline.

The existing savings endpoints use cached records for one configured endpoint; frontend
source and page controls remain unchanged. Recording and explicit content access use the API. Applied savings require
an explicit active mode and a unique captured association. Content fetching verifies the source
ID and original timestamp before updating a copy. The larger auth/savings handler files receive
only delegated entry points; their decomposition plan is to move auth handlers, savings DTOs and
tests into sibling modules while preserving routes. New transport and statistics selection logic stays in the
Tokenless modules. The Linux `make test-tokenless-api` gate exercises both built CLI processes;
build Tokenless and AgentSight first, and run it with Python 3.11 or newer.

Savings queries collect captured tool IDs, refresh the source in a blocking worker, save the
complete bounded result, then aggregate only source identities returned by this query. Both the
overview and session endpoint share this path. Deleted/replaced source rows can remain in the
historical cache without inflating fresh totals. An API failure fails the savings query; saved
metadata/content remain accessible separately. Configured savings queries require dashboard
credentials even on loopback because they use the stored service credential. The source requests
share a 5-second deadline, 1000-record/request cap and 8-MiB budget; duplicate tool IDs are fetched once.

Source matching batches up to 128 historical IDs through the authenticated Tokenless POST
query, preserving the complete turn map and compounding semantics. The shared budget limits
actual returned rows, response bytes, request count and elapsed time, rather than the number
of unmatched historical IDs. Saved-record lists require Dashboard credentials on loopback too.
