# Shared file-protection intent

This internal crate owns the backend-neutral file-protection data types used by
AgentSight and Lua-LSM. Cedar lowering lives in
`asc-policy-cedar::file_protection`; this crate has no Cedar dependency.
Each Translator checks which targets its backend can enforce. The SecCore
AgentSight adapter uses its existing wire types and media type and accepts
exact-file read targets. Its differential fixtures cover Cedar translation.
V1 Python and PAP keep their existing wire contracts.
