"""Exercise static descriptor discovery and transactional profile patching."""

from __future__ import annotations

import asyncio
import functools
import importlib
import inspect
import py_compile
import sys
import tempfile
import types
import unittest
from importlib.util import module_from_spec, spec_from_file_location
from pathlib import Path
from unittest.mock import AsyncMock, patch

from aw_sdk import AwError
from aw_sdk.profile_source import ProfileSource, function_code_sha256
from source_fixture import fake_source
from test_profile_eval import capability, document, target

MODULE = "aw_profile_patch_demo"


class ProfilePatchTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self) -> None:
        metadata = patch("aw_sdk.profile_source.version", return_value="1.4.0")
        metadata.start()
        self.addCleanup(metadata.stop)
        self.addCleanup(sys.modules.pop, MODULE, None)

    def eager_module(self) -> types.ModuleType:
        module = types.ModuleType(MODULE)

        def module_function() -> str:
            return "module"

        class Base:
            def instance(self) -> str:
                return type(self).__name__

            @staticmethod
            def static() -> str:
                return "static"

            @classmethod
            def klass(cls) -> str:
                return cls.__name__

        class Child(Base):
            pass

        module.module_function, module.Base, module.Child = module_function, Base, Child
        sys.modules[MODULE] = module
        return module

    async def test_patch_and_restore_function_and_descriptors(self) -> None:
        module = self.eager_module()
        original_fn = module.module_function
        original_static = inspect.getattr_static(module.Base, "static")
        original_class = inspect.getattr_static(module.Base, "klass")
        original_method = inspect.getattr_static(module.Base, "instance")
        source = ProfileSource.probe(
            document(
                *(
                    target(MODULE, name)
                    for name in (
                        "module_function",
                        "Base.static",
                        "Base.klass",
                        "Child.instance",
                    )
                )
            )
        )
        self.addAsyncCleanup(source.teardown)
        self.assertIs(module.module_function, original_fn)
        source.install()
        source.verify()
        self.assertIsNot(module.module_function, original_fn)
        self.assertIsInstance(
            inspect.getattr_static(module.Base, "static"), staticmethod
        )
        self.assertIsInstance(inspect.getattr_static(module.Base, "klass"), classmethod)
        self.assertIs(inspect.getattr_static(module.Base, "instance"), original_method)
        self.assertIn("instance", vars(module.Child))
        self.assertEqual(module.module_function(), "module")
        self.assertEqual(module.Child.static(), "static")
        self.assertEqual(module.Child.klass(), "Child")
        self.assertEqual(module.Child().instance(), "Child")
        await source.teardown()
        self.assertIs(module.module_function, original_fn)
        self.assertIs(inspect.getattr_static(module.Base, "static"), original_static)
        self.assertIs(inspect.getattr_static(module.Base, "klass"), original_class)
        self.assertIs(inspect.getattr_static(module.Child, "instance"), original_method)
        self.assertNotIn("instance", vars(module.Child))

    async def test_verify_keeps_owned_guards_until_teardown(self) -> None:
        module = self.eager_module()
        original = module.module_function
        original_base = module.Base
        static = inspect.getattr_static(original_base, "static")
        source = ProfileSource.probe(
            document(target(MODULE, "Base.static"), target(MODULE, "module_function"))
        )
        self.addAsyncCleanup(source.teardown)
        source.install()
        cached = module.module_function
        module.Base = type("Replacement", (), {})
        with self.assertRaisesRegex(AwError, "source_verify_failed"):
            source.verify()
        self.assertIs(module.module_function, cached)
        with self.assertRaisesRegex(AwError, "source_unavailable"):
            original_base.static()
        with self.assertRaisesRegex(AwError, "source_unavailable"):
            module.module_function()
        await source.teardown()
        self.assertIs(module.module_function, original)
        self.assertIs(inspect.getattr_static(original_base, "static"), static)
        with self.assertRaisesRegex(AwError, "source_unavailable"):
            cached()

    async def test_partial_install_obeys_availability_until_teardown(self) -> None:
        module = self.eager_module()
        original = module.module_function
        module.Builtin = str
        doc = document(
            target(MODULE, "module_function"), target(MODULE, "Builtin.lower")
        )
        for availability in ("native_fallback", "required"):
            source = ProfileSource.probe(doc, availability=availability)
            try:
                with self.assertRaisesRegex(AwError, "source_install_failed"):
                    source.install()
                if availability == "required":
                    with self.assertRaisesRegex(AwError, "source_unavailable"):
                        module.module_function()
                else:
                    self.assertIs(module.module_function, original)
            finally:
                await source.teardown()
            self.assertIs(module.module_function, original)

    def test_custom_method_descriptors_are_rejected_at_probe(self) -> None:
        module = self.eager_module()
        for kind in (classmethod, staticmethod):

            class Custom(kind):
                pass

            descriptor = Custom(lambda *args: None)
            module.Base.custom = descriptor
            with self.assertRaisesRegex(
                AwError, "source_probe_target_descriptor_unsupported"
            ):
                ProfileSource.probe(document(target(MODULE, "Base.custom")))
            self.assertIs(inspect.getattr_static(module.Base, "custom"), descriptor)

    async def test_module_creation_failure_invalidates_required_coverage(self) -> None:
        module = self.eager_module()
        lazy = "aw_profile_create_failure"

        class Loader:
            def find_spec(self, fullname, path, target=None):
                if fullname == lazy:
                    return importlib.util.spec_from_loader(fullname, self)

            def create_module(self, spec):
                raise ImportError("module unavailable")

            def exec_module(self, module):
                raise AssertionError("module was never created")

        with patch.object(sys, "meta_path", [Loader(), *sys.meta_path]):
            profile = ProfileSource.probe(
                document(target(MODULE, "module_function"), target(lazy))
            )
            profile.install()
            try:
                profile.verify()
                language, _ = fake_source(AsyncMock(), events=["session.start"])
                language.connect = AsyncMock(return_value=True)
                await profile.activate(language)
                with self.assertRaisesRegex(ImportError, "module unavailable"):
                    importlib.import_module(lazy)
                with self.assertRaisesRegex(AwError, "source_unavailable"):
                    module.module_function()
            finally:
                await profile.teardown()

    async def test_eager_custom_fingerprint(self) -> None:
        module = self.eager_module()
        seam = target(MODULE, "module_function", code_sha256="0" * 64)
        with self.assertRaisesRegex(AwError, "source_probe_fingerprint_mismatch"):
            ProfileSource.probe(document(seam, origin="custom"))
        seam["code_sha256"] = function_code_sha256(module.module_function)
        source = ProfileSource.probe(document(seam, origin="custom"))
        self.addAsyncCleanup(source.teardown)
        source.install()
        source.verify()

    async def test_lazy_sourceless_fingerprint_and_restoration(self) -> None:
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(sys, "path", [directory, *sys.path]),
        ):
            source = Path(directory) / f"{MODULE}.py"
            snippet = (
                "def run():\n    return [lambda: x in {1, 2, 3} for x in range(3)]\n"
            )
            source.write_text(snippet)
            spec = spec_from_file_location("operator_source", source)
            operator = module_from_spec(spec)
            spec.loader.exec_module(operator)
            digest = function_code_sha256(operator.run)
            py_compile.compile(
                str(source), cfile=str(source.with_suffix(".pyc")), doraise=True
            )
            source.unlink()
            profile = ProfileSource.probe(
                document(target(MODULE, code_sha256=digest), origin="custom")
            )
            self.addAsyncCleanup(profile.teardown)
            self.assertNotIn(MODULE, sys.modules)
            profile.install()
            profile.verify()
            module = importlib.import_module(MODULE)
            profile.verify()
            self.assertEqual(function_code_sha256(module.run), digest)
            self.assertIsNot(module.run, inspect.unwrap(module.run))
            with self.assertRaisesRegex(
                AwError, "source_probe_target_reload_unsupported"
            ):
                importlib.reload(module)
            original = inspect.unwrap(module.run)
            await profile.teardown()
            self.assertIs(module.run, original)

    async def test_lazy_mismatch_poisoning_and_native_fallback(self) -> None:
        module = self.eager_module()
        original = module.module_function
        lazy_name = "aw_profile_patch_lazy_failure"
        self.addCleanup(sys.modules.pop, lazy_name, None)
        fingerprint = "0" * 64
        doc = document(
            target(
                MODULE, "module_function", code_sha256=function_code_sha256(original)
            ),
            target(lazy_name, code_sha256=fingerprint),
            origin="custom",
        )
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(sys, "path", [directory, *sys.path]),
        ):
            (Path(directory) / f"{lazy_name}.py").write_text(
                "def run(): return 'native'\n"
            )
            for active in (False, True):
                for availability in ("required", "native_fallback"):
                    source = ProfileSource.probe(doc, availability=availability)
                    self.addAsyncCleanup(source.teardown)
                    source.install()
                    source.verify()
                    if active:
                        language, _ = fake_source(
                            AsyncMock(),
                            events=["session.start"],
                            availability=availability,
                        )
                        language.connect = AsyncMock(return_value=True)
                        await source.activate(language)
                    cached = module.module_function
                    if availability == "required":
                        with self.assertRaisesRegex(
                            AwError, "source_probe_fingerprint_mismatch"
                        ):
                            importlib.import_module(lazy_name)
                        with self.assertRaisesRegex(AwError, "source_unavailable"):
                            importlib.import_module(lazy_name)
                        with self.assertRaisesRegex(AwError, "source_unavailable"):
                            cached()
                    else:
                        self.assertEqual(
                            importlib.import_module(lazy_name).run(), "native"
                        )
                        self.assertEqual(cached(), "module")
                        if active:
                            async with source.request() as session:
                                self.assertIsNone(session)
                                self.assertEqual(module.module_function(), "module")
                    if availability == "required":
                        self.assertIs(module.module_function, cached)
                        with self.assertRaisesRegex(AwError, "source_unavailable"):
                            module.module_function()
                    else:
                        self.assertIs(module.module_function, original)
                    await source.teardown()
                    self.assertIs(module.module_function, original)
                    sys.modules.pop(lazy_name, None)

    async def test_lazy_function_classification_and_method_binding(self) -> None:
        module = self.eager_module()

        class Agent:
            async def complete(self):
                return self

            @classmethod
            async def chunks(cls):
                value = yield cls
                yield value

            @staticmethod
            def tokens():
                yield "token"

            @staticmethod
            def iterator():
                return iter(("item",))

        module.Agent = Agent
        shapes = {
            "complete": "async",
            "chunks": "async_iter",
            "tokens": "sync_iter",
            "iterator": "sync_iter",
        }
        profile = ProfileSource.probe(
            document(
                *(
                    target(
                        MODULE,
                        f"Agent.{name}",
                        call=shape,
                        hooks={"session.start": capability(actions=("report",))},
                    )
                    for name, shape in shapes.items()
                )
            )
        )
        self.addAsyncCleanup(profile.teardown)
        agent = Agent()

        def classification(fn):
            return (
                inspect.iscoroutinefunction(fn),
                inspect.isasyncgenfunction(fn),
                inspect.isgeneratorfunction(fn),
                inspect.signature(fn),
            )

        expected = [classification(getattr(agent, name)) for name in shapes]
        profile.install()
        profile.verify()
        self.assertEqual(
            [classification(getattr(agent, name)) for name in shapes], expected
        )
        language, _ = fake_source(
            AsyncMock(return_value={"decision": "continue"}), events=["session.start"]
        )
        language.connect = AsyncMock(return_value=True)
        await profile.activate(language)
        async with profile.request():
            self.assertIs(await agent.complete(), agent)
            stream = agent.chunks()
            self.assertIs(await anext(stream), Agent)
            self.assertEqual(await stream.asend("next"), "next")
            await stream.aclose()
            self.assertEqual(
                await asyncio.to_thread(lambda: list(agent.tokens())), ["token"]
            )
            self.assertEqual(
                await asyncio.to_thread(lambda: list(agent.iterator())), ["item"]
            )

    def test_code_hash_ignores_locations_and_decorators(self) -> None:
        def native(value: float = 1.0) -> tuple:
            return (value, b"data", 2j, ...)

        @functools.wraps(native)
        def same(*args: object, **kwargs: object) -> tuple:
            return native(*args, **kwargs)

        new_location = native.__code__.replace(
            co_filename="/different/path.py", co_firstlineno=900
        )
        relocated = types.FunctionType(new_location, native.__globals__)
        digest = function_code_sha256(native)
        self.assertRegex(digest, r"^[a-f0-9]{64}$")
        self.assertEqual(function_code_sha256(same), digest)
        self.assertEqual(function_code_sha256(relocated), digest)
        self.assertNotEqual(function_code_sha256(lambda: b"different"), digest)
