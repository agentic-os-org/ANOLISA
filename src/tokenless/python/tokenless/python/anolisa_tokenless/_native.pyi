import os

class TokenlessError(Exception): ...

class _StatsQuery:
    def __init__(self, data_dir: str | os.PathLike[str] | None = None) -> None: ...
    def status_json(self) -> str: ...
    def summary_json(self, limit: int | None = None) -> str: ...
    def list_json(self, limit: int = 20) -> str: ...
    def show_json(self, record_id: int) -> str | None: ...
    def diff_json(
        self,
        *,
        record_id: int | None = None,
        session_id: str | None = None,
        tool_use_id: str | None = None,
        limit: int = 20,
        sort: str = "saved",
        context: int = 3,
    ) -> str | None: ...
    def compare_json(
        self,
        baseline_session_id: str,
        tokenless_session_id: str,
        limit: int | None = None,
    ) -> str: ...

class CompressionResult:
    @property
    def output(self) -> str: ...
    @property
    def compressed_output(self) -> str: ...
    @property
    def disposition(self) -> str: ...
    @property
    def applied(self) -> bool: ...
    @property
    def before_tokens(self) -> int: ...
    @property
    def after_tokens(self) -> int: ...
    @property
    def stash_writes(self) -> int | None: ...
    @property
    def stash_errors(self) -> int | None: ...
    @property
    def unrecoverable_truncations(self) -> int | None: ...
    @property
    def stash_size(self) -> int | None: ...

class TokenlessRuntime:
    def __init__(
        self,
        data_dir: str | os.PathLike[str] | None = None,
        *,
        compression_enabled: bool = True,
        search_path_sharing_enabled: bool = True,
        diff_compression_enabled: bool = False,
        stats_enabled: bool = True,
        sls_enabled: bool = False,
    ) -> None: ...
    def _before_model_json(
        self,
        request_json: str,
        *,
        agent_id: str = "python",
        session_id: str | None = None,
        tool_use_id: str | None = None,
    ) -> str: ...
    def _pre_tool_json(
        self,
        request_json: str,
        rtk_path: str | os.PathLike[str],
        *,
        agent_id: str = "python",
        session_id: str | None = None,
        tool_use_id: str | None = None,
    ) -> str: ...
    def _post_tool_json(
        self,
        request_json: str,
        *,
        agent_id: str = "python",
        session_id: str | None = None,
        tool_use_id: str | None = None,
    ) -> str: ...
    def _retrieve_authorized_json(
        self,
        request_json: str,
        *,
        agent_id: str = "python",
        session_id: str | None = None,
        tool_use_id: str | None = None,
    ) -> str: ...
    def compress_response(
        self,
        input: str,
        *,
        truncate_strings_at: int | None = None,
        truncate_arrays_at: int | None = None,
        max_depth: int | None = None,
        agent_id: str = "python",
        session_id: str | None = None,
        tool_use_id: str | None = None,
        stash_enabled: bool = True,
        require_reversible: bool = True,
    ) -> CompressionResult: ...
    def compress_schema(
        self,
        input: str,
        *,
        agent_id: str = "python",
        session_id: str | None = None,
        tool_use_id: str | None = None,
    ) -> CompressionResult: ...
    def compress_toon(
        self,
        input: str,
        *,
        agent_id: str = "python",
        session_id: str | None = None,
        tool_use_id: str | None = None,
    ) -> CompressionResult: ...
    def retrieve(self, hash_or_marker: str) -> str: ...
    @property
    def data_dir(self) -> str: ...
    @property
    def stash_available(self) -> bool: ...
    @property
    def stash_error(self) -> str | None: ...
    @property
    def stats_available(self) -> bool: ...
    @property
    def stats_error(self) -> str | None: ...

__version__: str
