#!/usr/bin/env python3
"""Regression tests for the Hermes plugin hook_utils resolution.

Covers the review findings on PR #2058 and PR #2249:
- P1-a: the hooks directory itself, its parent, and hook_utils.py must all
  be rejected when world-writable or foreign-owned (not just the parent).
- P1-b: copy-installs must honor XDG_DATA_HOME (anolisa FsLayout::user
  prefers it over ~/.local/share).
- P1-c: an existing-but-incomplete high-priority candidate must not stop
  the search; later valid candidates are still tried.
- P2: candidate list contains no empty placeholders; _validate_hooks_dir
  rejects relative/empty paths; the ImportError mentions trust-policy
  rejections, not just "missing".
- PR #2249 P1: a trusted candidate shipping an older hook_utils.py that
  lacks the Protocol v2 lifecycle symbols — or a v2-era module without the
  newer Retrieve helpers — must be rejected by the API compatibility check
  so the search continues to later candidates.
- PR #2249 P2 (re-review): a trusted candidate whose hook_utils.py exports
  every required symbol but with an older *call signature* — the
  pre-c2c7e580e build_post_tool_request that takes keyword-only
  `retrieval_available` instead of `recovery` — must be rejected by the same
  check, so the search continues to later candidates instead of the plugin
  importing cleanly and then failing every PostTool request.

After the Hermes lifecycle migration the adapter is a thin Core client:
RTK execution and rtk-prefix anchoring are owned by tokenless-runtime.
There is no degraded mode anymore — an incompatible hook_utils
fails the plugin import with a diagnostic instead.
"""

import importlib.util
import os
import shutil
import sys
import tempfile
import unittest

_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
_PLUGIN_SRC = os.path.join(_REPO_ROOT, "adapters", "tokenless", "hermes", "__init__.py")
_HOOKS_SRC = os.path.join(_REPO_ROOT, "adapters", "tokenless", "common", "hooks")


def _load_plugin(path: str, name: str):
    """Load a copy of the Hermes plugin module under a unique name."""
    # Drop any previously imported hook_utils so each load re-resolves it.
    sys.modules.pop("hook_utils", None)
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    pre_path = sys.path[:]
    try:
        spec.loader.exec_module(module)
    finally:
        sys.path[:] = pre_path
    return module


def _make_hooks_dir(base: str) -> str:
    """Create a complete, trusted hooks dir under base and return its path."""
    hooks = os.path.join(base, "anolisa", "adapters", "tokenless", "common", "hooks")
    os.makedirs(hooks, mode=0o755)
    for fname in ("hook_utils.py", "tool_categories.json"):
        shutil.copy(os.path.join(_HOOKS_SRC, fname), hooks)
    os.chmod(hooks, 0o755)
    return hooks


class ValidateHooksDirTest(unittest.TestCase):
    """Unit tests for _validate_hooks_dir (loaded from the source tree)."""

    @classmethod
    def setUpClass(cls):
        # Source-tree import: the relative candidate resolves, so loading
        # the real plugin file always succeeds here.
        cls.plugin = _load_plugin(_PLUGIN_SRC, "hermes_plugin_srctree")

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="hermes-hooks-test-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)

    def test_rejects_empty_and_relative_paths(self):
        self.assertIsNotNone(self.plugin._validate_hooks_dir(""))
        self.assertIsNotNone(self.plugin._validate_hooks_dir("relative/hooks"))

    def test_rejects_missing_directory(self):
        reason = self.plugin._validate_hooks_dir(os.path.join(self.tmp, "nope"))
        self.assertIn("does not exist", reason)

    def test_rejects_incomplete_dir_without_hook_utils(self):
        # P1-c: uninstall residue — dir exists but hook_utils.py is gone.
        empty = os.path.join(self.tmp, "hooks")
        os.makedirs(empty)
        reason = self.plugin._validate_hooks_dir(empty)
        self.assertIn("hook_utils.py missing", reason)

    def test_accepts_trusted_complete_dir(self):
        hooks = _make_hooks_dir(self.tmp)
        self.assertIsNone(self.plugin._validate_hooks_dir(hooks))

    def test_rejects_world_writable_hooks_dir(self):
        # P1-a: the hooks dir itself is world-writable.
        hooks = _make_hooks_dir(self.tmp)
        os.chmod(hooks, 0o777)
        reason = self.plugin._validate_hooks_dir(hooks)
        self.assertIn("world-writable", reason)

    def test_rejects_world_writable_hook_utils_file(self):
        # P1-a: hook_utils.py itself is world-writable (0666).
        hooks = _make_hooks_dir(self.tmp)
        os.chmod(os.path.join(hooks, "hook_utils.py"), 0o666)
        reason = self.plugin._validate_hooks_dir(hooks)
        self.assertIn("world-writable", reason)

    def test_rejects_world_writable_parent_dir(self):
        hooks = _make_hooks_dir(self.tmp)
        os.chmod(os.path.dirname(hooks), 0o777)
        reason = self.plugin._validate_hooks_dir(hooks)
        self.assertIn("world-writable", reason)

    def test_candidate_list_has_no_empty_entries(self):
        # P2: no "" placeholder elements in the candidate list.
        for candidate in self.plugin._HOOK_UTILS_CANDIDATES:
            self.assertTrue(candidate, "empty candidate in _HOOK_UTILS_CANDIDATES")
            self.assertTrue(os.path.isabs(candidate) or candidate.startswith(self.plugin._HERE))


class CopyInstallResolutionTest(unittest.TestCase):
    """End-to-end: plugin copied to a bare dir (anolisa driver behavior)."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="hermes-copy-test-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        plugin_dir = os.path.join(self.tmp, "plugins", "tokenless")
        os.makedirs(plugin_dir)
        shutil.copy(_PLUGIN_SRC, plugin_dir)
        self.plugin_copy = os.path.join(plugin_dir, "__init__.py")
        self._saved_xdg = os.environ.get("XDG_DATA_HOME")

    def tearDown(self):
        if self._saved_xdg is None:
            os.environ.pop("XDG_DATA_HOME", None)
        else:
            os.environ["XDG_DATA_HOME"] = self._saved_xdg

    def test_resolves_via_xdg_data_home(self):
        # P1-b: XDG_DATA_HOME layout must be honored for copy-installs.
        xdg = os.path.join(self.tmp, "xdg-data")
        hooks = _make_hooks_dir(xdg)
        os.environ["XDG_DATA_HOME"] = xdg
        plugin = _load_plugin(self.plugin_copy, "hermes_plugin_xdg")
        self.assertEqual(plugin._HOOK_UTILS_RESOLVED, os.path.realpath(hooks))

    def test_incomplete_xdg_candidate_does_not_mask_later_ones(self):
        # P1-c: an existing-but-empty XDG hooks dir must be skipped, and the
        # search must continue to later candidates instead of breaking.
        xdg = os.path.join(self.tmp, "xdg-data")
        empty_hooks = os.path.join(xdg, "anolisa", "adapters", "tokenless", "common", "hooks")
        os.makedirs(empty_hooks)
        os.environ["XDG_DATA_HOME"] = xdg
        try:
            plugin = _load_plugin(self.plugin_copy, "hermes_plugin_incomplete_xdg")
        except ImportError as exc:
            # No later candidate exists on this machine — the diagnostic must
            # name the incomplete dir with its rejection reason (P2 wording).
            self.assertIn("hook_utils.py missing", str(exc))
            self.assertIn(empty_hooks, str(exc))
        else:
            # A later candidate (e.g. passwd-home install) won — but never
            # the incomplete XDG dir.
            self.assertNotEqual(plugin._HOOK_UTILS_RESOLVED, os.path.realpath(empty_hooks))

    def test_import_error_mentions_trust_policy(self):
        # P2: the diagnostic must explain that existing paths can be
        # rejected by the trust policy, not only be "missing".
        xdg = os.path.join(self.tmp, "xdg-data")
        hooks = _make_hooks_dir(xdg)
        os.chmod(hooks, 0o777)  # exists but untrusted
        os.environ["XDG_DATA_HOME"] = xdg
        try:
            plugin = _load_plugin(self.plugin_copy, "hermes_plugin_untrusted_xdg")
        except ImportError as exc:
            self.assertIn("world-writable", str(exc))
            self.assertIn("trust policy", str(exc))
        else:
            # Later candidate won; the untrusted dir must not be selected.
            self.assertNotEqual(plugin._HOOK_UTILS_RESOLVED, os.path.realpath(hooks))


class VersionMismatchTest(unittest.TestCase):
    """Regression tests for shared hook_utils version mismatch (PR #2249 P1).

    When a candidate passes the trust check (hook_utils.py exists, ownership
    and permissions OK) but ships an older hook_utils that lacks the
    Protocol v2 lifecycle symbols this adapter imports
    (build_pre_tool_request, build_post_tool_request, run_compress,
    is_tokenless_retrieve_command, tokenless_retrieve_command_available), the
    candidate must be rejected with an "API mismatch" reason and the search
    must continue to later candidates. When no compatible candidate exists,
    the plugin import fails with a diagnostic — there is no degraded mode
    anymore, because the lifecycle adapter delegates every feature to Core.

    Symbol names alone are not a sufficient contract (PR #2249 P2
    re-review): hook_utils replaced build_post_tool_request's keyword-only
    `retrieval_available` with `recovery` in c2c7e580e, so the module from
    9f109d559 exports all five required symbols and still cannot take the
    adapter's PostTool call — it would be imported and then raise
    "unexpected keyword argument 'recovery'" on the first request, with the
    candidate search already finished. Those signature-skewed modules must be
    rejected up front as well.

    Also covers _check_api_compat's sys.modules behavior on success
    (PR #2249 P2): the freshly imported module must stay cached instead
    of a stale copy being restored.
    """

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="hermes-version-test-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        plugin_dir = os.path.join(self.tmp, "plugins", "tokenless")
        os.makedirs(plugin_dir)
        shutil.copy(_PLUGIN_SRC, plugin_dir)
        self.plugin_copy = os.path.join(plugin_dir, "__init__.py")
        self._saved_xdg = os.environ.get("XDG_DATA_HOME")

    def tearDown(self):
        if self._saved_xdg is None:
            os.environ.pop("XDG_DATA_HOME", None)
        else:
            os.environ["XDG_DATA_HOME"] = self._saved_xdg

    def _make_old_hooks_dir(self, base: str) -> str:
        """Create a hooks dir whose hook_utils.py lacks the lifecycle API."""
        hooks = os.path.join(base, "anolisa", "adapters", "tokenless", "common", "hooks")
        os.makedirs(hooks, mode=0o755)
        # Write a minimal hook_utils.py without the Protocol v2 builders
        # (pre-lifecycle installs shipped exactly this shape).
        with open(os.path.join(hooks, "hook_utils.py"), "w") as f:
            f.write(
                "# Old hook_utils without the Protocol v2 lifecycle API\n"
                "def resolve_binary(name, *fallbacks): return None\n"
            )
        # Copy tool_categories.json (needed by some imports)
        shutil.copy(
            os.path.join(_HOOKS_SRC, "tool_categories.json"),
            hooks,
        )
        os.chmod(hooks, 0o755)
        return hooks

    def _make_v2_era_hooks_dir(self, base: str) -> str:
        """Create a hooks dir whose hook_utils.py has the Protocol v2
        lifecycle builders but predates the Retrieve helpers (the shape
        shipped between the lifecycle migration and the Retrieve feature).
        """
        hooks = os.path.join(base, "anolisa", "adapters", "tokenless", "common", "hooks")
        os.makedirs(hooks, mode=0o755)
        with open(os.path.join(hooks, "hook_utils.py"), "w") as f:
            f.write(
                "# v2-era hook_utils: lifecycle API present, Retrieve helpers absent\n"
                "def resolve_binary(name, *fallbacks): return None\n"
                "def build_pre_tool_request(*args, **kwargs): return {}\n"
                "def build_post_tool_request(*args, **kwargs): return {}\n"
                "def run_compress(*args, **kwargs): return None\n"
            )
        shutil.copy(
            os.path.join(_HOOKS_SRC, "tool_categories.json"),
            hooks,
        )
        os.chmod(hooks, 0o755)
        return hooks

    def _write_signature_skew_module(self, hooks: str) -> str:
        """Write a faithful stand-in for the hook_utils.py that 9f109d559
        shipped: every symbol this adapter from-imports at load time is
        present, and four of the five required callables keep their current
        signature — only build_post_tool_request still takes the
        pre-c2c7e580e keyword-only `retrieval_available` instead of
        `recovery`.  Such a module imports cleanly, so a name-only
        compatibility check accepts it and the failure only surfaces on the
        first PostTool request.
        """
        os.makedirs(hooks, mode=0o755)
        with open(os.path.join(hooks, "hook_utils.py"), "w") as f:
            f.write(
                "# v2-era hook_utils, PostTool signature predates c2c7e580e\n"
                "SHELL_TOOLS = {'shell', 'terminal'}\n"
                "SKIP_TOOLS = set()\n"
                "_RTK_FALLBACK = _RTK_LOCAL_LIB = _RTK_LOCAL_SHARE = ''\n"
                "_TOKENLESS_FALLBACK = _TOKENLESS_LOCAL_LIB = ''\n"
                "_TOKENLESS_LOCAL_SHARE = ''\n"
                "def resolve_binary(name, *fallbacks): return None\n"
                "def build_pre_tool_request(*args, **kwargs): return {}\n"
                "def build_post_tool_request(\n"
                "    content, agent_id, tool_name, status, content_origin,\n"
                "    output_optimization, *, result_kind, retrieval_available,\n"
                "    session_id='', tool_use_id='', replace_output=False,\n"
                "    replace_with_text=False,\n"
                "): return {}\n"
                "def run_compress(*args, **kwargs): return None\n"
                "def is_tokenless_retrieve_command(*args, **kwargs): return False\n"
                "def tokenless_retrieve_command_available(): return False\n"
            )
        shutil.copy(
            os.path.join(_HOOKS_SRC, "tool_categories.json"),
            hooks,
        )
        os.chmod(hooks, 0o755)
        return hooks

    def _make_signature_skew_hooks_dir(self, base: str) -> str:
        """Create a signature-skewed hooks dir in the anolisa data layout."""
        hooks = os.path.join(base, "anolisa", "adapters", "tokenless", "common", "hooks")
        return self._write_signature_skew_module(hooks)

    def test_old_hooks_rejected_by_api_compat_check(self):
        # A candidate whose hook_utils.py lacks the lifecycle builders must
        # be rejected by _check_api_compat, not accepted and then crash on
        # the top-level from-import.
        xdg = os.path.join(self.tmp, "xdg-data")
        old_hooks = self._make_old_hooks_dir(xdg)
        os.environ["XDG_DATA_HOME"] = xdg
        try:
            plugin = _load_plugin(self.plugin_copy, "hermes_plugin_old_hooks")
        except ImportError as exc:
            # No compatible candidate found — diagnostic mentions API mismatch.
            self.assertIn("API mismatch", str(exc))
            self.assertIn(old_hooks, str(exc))
        else:
            # A later candidate with the correct version won.
            self.assertNotEqual(plugin._HOOK_UTILS_RESOLVED, os.path.realpath(old_hooks))

    def test_no_compat_candidate_fails_with_api_mismatch(self):
        # When no candidate provides the Protocol v2 symbols, the plugin
        # must fail loudly with the API mismatch diagnostic instead of
        # silently importing a stale hook_utils. (The pre-lifecycle version
        # degraded to local fallbacks; Core now owns every feature, so an
        # adapter without the lifecycle API has nothing useful to do.)
        xdg = os.path.join(self.tmp, "xdg-data")
        old_hooks = self._make_old_hooks_dir(xdg)
        os.environ["XDG_DATA_HOME"] = xdg
        try:
            plugin = _load_plugin(self.plugin_copy, "hermes_plugin_incompat")
        except ImportError as exc:
            msg = str(exc)
            self.assertIn("API mismatch", msg)
            self.assertIn(old_hooks, msg)
            self.assertIn("build_pre_tool_request", msg)
        else:
            # A later, newer candidate won — never the stale one.
            self.assertNotEqual(plugin._HOOK_UTILS_RESOLVED, os.path.realpath(old_hooks))

    def test_v2_era_hooks_missing_retrieve_helpers_rejected(self):
        # A historical module that already ships the Protocol v2 builders
        # but predates the Retrieve helpers must be rejected too — otherwise
        # it passes _check_api_compat and the plugin then dies on the
        # top-level from-import of is_tokenless_retrieve_command /
        # tokenless_retrieve_command_available, after the candidate search
        # has ended, with no fallback to later complete candidates.
        xdg = os.path.join(self.tmp, "xdg-data")
        old_hooks = self._make_v2_era_hooks_dir(xdg)
        os.environ["XDG_DATA_HOME"] = xdg
        try:
            plugin = _load_plugin(self.plugin_copy, "hermes_plugin_v2_era")
        except ImportError as exc:
            msg = str(exc)
            self.assertIn("API mismatch", msg)
            self.assertIn("is_tokenless_retrieve_command", msg)
            self.assertIn(old_hooks, msg)
        else:
            # A later, complete candidate won — never the v2-era one.
            self.assertNotEqual(plugin._HOOK_UTILS_RESOLVED, os.path.realpath(old_hooks))

    def test_check_api_compat_rejects_v2_era_module(self):
        # Deterministic direct probe: a module with the three Protocol v2
        # symbols but without the Retrieve helpers must be rejected, listing
        # both missing helpers, and the previously cached complete module
        # must stay in sys.modules untouched.
        plugin = _load_plugin(_PLUGIN_SRC, "hermes_api_compat_v2_era")
        v2_era = self._make_v2_era_hooks_dir(os.path.join(self.tmp, "v2-era"))
        cached = sys.modules.get("hook_utils")
        self.assertIsNotNone(cached, "plugin load must leave hook_utils cached")
        reason = plugin._check_api_compat(v2_era)
        self.assertIsNotNone(reason, "v2-era module must not pass the API check")
        self.assertIn("API mismatch", reason)
        self.assertIn("is_tokenless_retrieve_command", reason)
        self.assertIn("tokenless_retrieve_command_available", reason)
        # The rejected trial import must not clobber the cached module.
        self.assertIs(sys.modules.get("hook_utils"), cached)

    def test_check_api_compat_keeps_fresh_module_on_success(self):
        # When _check_api_compat succeeds, the freshly imported module must
        # stay in sys.modules — the old cached module must NOT be restored.
        import types

        plugin = _load_plugin(_PLUGIN_SRC, "hermes_api_compat_cache")

        # Create a fake "old" module and put it in sys.modules
        old_mod = types.ModuleType("hook_utils")
        old_mod._STALE = True  # marker so we can detect it
        sys.modules["hook_utils"] = old_mod

        try:
            # Trial-import from the real source-tree hooks dir
            hooks_dir = os.path.realpath(_HOOKS_SRC)
            reason = plugin._check_api_compat(hooks_dir)
            self.assertIsNone(reason, "compatible candidate must pass API check")

            # The module in sys.modules must be the fresh one, not old_mod
            current = sys.modules.get("hook_utils")
            self.assertIsNotNone(current, "module must remain in sys.modules")
            self.assertFalse(
                getattr(current, "_STALE", False),
                "stale module was restored instead of fresh candidate",
            )
        finally:
            sys.modules.pop("hook_utils", None)

    def test_check_api_compat_rejects_signature_skewed_module(self):
        # Deterministic direct probe: all five required symbols are present,
        # but build_post_tool_request still takes `retrieval_available`, so
        # the candidate must be rejected on its call shape instead of being
        # accepted and blowing up on the first PostTool request.
        plugin = _load_plugin(_PLUGIN_SRC, "hermes_api_compat_signature_skew")
        skewed = self._make_signature_skew_hooks_dir(os.path.join(self.tmp, "skew"))
        cached = sys.modules.get("hook_utils")
        self.assertIsNotNone(cached, "plugin load must leave hook_utils cached")
        reason = plugin._check_api_compat(skewed)
        self.assertIsNotNone(
            reason, "signature-skewed module must not pass the API check"
        )
        self.assertIn("API mismatch", reason)
        self.assertIn("build_post_tool_request", reason)
        self.assertIn("recovery", reason)
        # The rejected trial import must not clobber the cached module.
        self.assertIs(sys.modules.get("hook_utils"), cached)

    def test_signature_skew_candidate_does_not_mask_later_complete_one(self):
        # End-to-end repro of the review finding: the signature-skewed module
        # sits in the highest-priority candidate (the source-tree relative
        # path next to the copied plugin) and the complete current module sits
        # in a later XDG candidate.  The search must skip the skewed one, and
        # the selected module must really take the adapter's PostTool call.
        skewed = self._write_signature_skew_module(
            os.path.join(self.tmp, "plugins", "common", "hooks")
        )
        xdg = os.path.join(self.tmp, "xdg-data")
        complete = _make_hooks_dir(xdg)
        os.environ["XDG_DATA_HOME"] = xdg

        plugin = _load_plugin(self.plugin_copy, "hermes_plugin_signature_skew")

        self.assertEqual(plugin._HOOK_UTILS_RESOLVED, os.path.realpath(complete))
        self.assertNotEqual(plugin._HOOK_UTILS_RESOLVED, os.path.realpath(skewed))
        request = plugin.build_post_tool_request(
            "model-visible output",
            plugin.AGENT_ID,
            "shell",
            "success",
            "tool",
            "none",
            result_kind="tool",
            recovery={"kind": "shell"},
            session_id="session-1",
            tool_use_id="call-1",
            replace_output=True,
            replace_with_text=True,
        )
        self.assertEqual(request["protocol_version"], 2)
        self.assertEqual(request["operation"], "post_tool")
        self.assertEqual(
            request["input"]["capabilities"]["recovery"], {"kind": "shell"}
        )

    def test_signature_skew_only_candidate_fails_with_diagnostic(self):
        # When the only trusted candidate is signature-skewed, the plugin must
        # fail loudly with the API mismatch diagnostic rather than import a
        # module whose PostTool builder cannot take `recovery=`.
        xdg = os.path.join(self.tmp, "xdg-data")
        skewed = self._make_signature_skew_hooks_dir(xdg)
        os.environ["XDG_DATA_HOME"] = xdg
        try:
            plugin = _load_plugin(self.plugin_copy, "hermes_plugin_skew_only")
        except ImportError as exc:
            msg = str(exc)
            self.assertIn("API mismatch", msg)
            self.assertIn("build_post_tool_request", msg)
            self.assertIn(skewed, msg)
        else:
            # A later, complete candidate won — never the skewed one.
            self.assertNotEqual(plugin._HOOK_UTILS_RESOLVED, os.path.realpath(skewed))



if __name__ == "__main__":
    unittest.main()
