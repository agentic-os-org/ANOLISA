"""Task resolution uses filesystem kind consistently at all three entry points."""

import importlib.util
import sys
from pathlib import Path
from types import ModuleType, SimpleNamespace

import pytest

ROOT = Path(__file__).parents[1]


class TaskLoaded(Exception):
    pass


@pytest.fixture
def entrypoints(monkeypatch: pytest.MonkeyPatch, tmp_path: Path):
    package = ModuleType("ce_runner")
    package.__path__ = [str(ROOT / "src" / "ce_runner")]
    package.__version__ = "test"
    monkeypatch.setitem(sys.modules, "ce_runner", package)
    modules = {}
    for name in ("_common", "infra", "sandbox", "batch_runner"):
        module = ModuleType(f"ce_runner.{name}")
        monkeypatch.setitem(sys.modules, module.__name__, module)
        modules[name] = module
    common = modules["_common"]
    common._REPO_DIR = tmp_path / "repo"
    common._PYTHON = sys.executable
    common.OPENCLAW_CONFIG = tmp_path / "config.json"
    common.DEFAULT_AGENT_TIMEOUT_S = 600
    common.load_config = lambda *args: {}
    common.log = lambda *args: None
    common.make_trace_dir = lambda *args, **kwargs: None
    common.require_valid_config = lambda *args: None
    loaded = []

    def load_task(path):
        Path(path).read_text(encoding="utf-8")
        loaded.append(Path(path).resolve())
        raise TaskLoaded

    common.load_task_yaml = load_task
    for name in (
        "check_gateway",
        "configure_tools",
        "cleanup_mock_services",
        "reset_services",
        "start_mock_services",
        "restart_gateway",
        "cleanup_config",
    ):
        setattr(
            modules["infra"],
            name,
            lambda *args, **kwargs: pytest.fail(
                "resolution must precede infrastructure"
            ),
        )
    modules["sandbox"].execute_task_sandbox = lambda *args: None
    modules["sandbox"].convert_and_grade_sandbox = lambda *args: None
    monkeypatch.setattr(sys, "path", list(sys.path))

    def load_module(name, path):
        spec = importlib.util.spec_from_file_location(name, path)
        module = importlib.util.module_from_spec(spec)
        monkeypatch.setitem(sys.modules, name, module)
        spec.loader.exec_module(module)
        return module

    runner = load_module("ce_runner.run_task", ROOT / "src/ce_runner/run_task.py")
    debug = load_module("resolution_debug", ROOT / "scripts/debug_task.py")
    prompt = load_module("resolution_prompt", ROOT / "scripts/prompt_task.py")

    def run(entry, value):
        if entry == "runner":
            return runner.resolve_task(value)
        if entry == "debug":
            debug.run_debug(
                SimpleNamespace(task=value, config=None, sandbox_image=None)
            )
        else:
            monkeypatch.setattr(sys, "argv", ["prompt_task.py", value])
            prompt.main()

    return SimpleNamespace(
        run=run, loaded=loaded, tasks=common._REPO_DIR / "claw-eval/tasks"
    )


@pytest.mark.parametrize("entry", ["debug", "prompt"])
@pytest.mark.parametrize(
    "kind",
    [
        "yaml_file",
        "yml_file",
        "directory",
        "yaml_directory",
        "bare_directory",
        "bare_yml_file",
    ],
)
def test_interactive_paths_reach_the_existing_task_file(
    entrypoints, tmp_path: Path, entry: str, kind: str
):
    if kind.startswith("bare"):
        entrypoints.tasks.mkdir(parents=True)
        root = entrypoints.tasks
    else:
        root = tmp_path
    if kind in {"yaml_file", "yml_file", "bare_yml_file"}:
        suffix = ".yaml" if kind == "yaml_file" else ".yml"
        task = root / f"T001{suffix}"
        task.write_text("task_id: T001\n", encoding="utf-8")
        value = task.name if kind.startswith("bare") else str(task)
    else:
        directory = root / ("T001.yaml" if kind == "yaml_directory" else "T001")
        directory.mkdir()
        task = directory / "task.yaml"
        task.write_text("task_id: T001\n", encoding="utf-8")
        value = directory.name if kind.startswith("bare") else str(directory)
    with pytest.raises(TaskLoaded):
        entrypoints.run(entry, value)
    assert entrypoints.loaded == [task.resolve()]


@pytest.mark.parametrize("entry", ["debug", "prompt"])
def test_existing_local_task_takes_precedence_over_default_root(
    entrypoints, tmp_path: Path, monkeypatch, entry
):
    monkeypatch.chdir(tmp_path)
    local = tmp_path / "T001"
    default = entrypoints.tasks / "T001"
    local.mkdir()
    default.mkdir(parents=True)
    (local / "task.yaml").write_text("task_id: local\n", encoding="utf-8")
    (default / "task.yaml").write_text("task_id: default\n", encoding="utf-8")
    with pytest.raises(TaskLoaded):
        entrypoints.run(entry, "T001")
    assert entrypoints.loaded == [(local / "task.yaml").resolve()]


@pytest.mark.parametrize("entry", ["runner", "debug", "prompt"])
def test_missing_task_exits_before_infrastructure(entrypoints, entry):
    with pytest.raises(SystemExit) as raised:
        entrypoints.run(entry, "missing")
    assert raised.value.code == 1
    assert entrypoints.loaded == []


@pytest.mark.parametrize("entry", ["runner", "debug", "prompt"])
def test_directory_without_task_file_exits_before_loading(
    entrypoints, tmp_path: Path, entry
):
    directory = tmp_path / "empty"
    directory.mkdir()
    with pytest.raises(SystemExit) as raised:
        entrypoints.run(entry, str(directory))
    assert raised.value.code == 1
    assert entrypoints.loaded == []


@pytest.mark.parametrize("kind", ["yaml", "yml", "directory"])
def test_runner_retains_absolute_path_contract(
    entrypoints, tmp_path: Path, monkeypatch, kind
):
    monkeypatch.chdir(tmp_path)
    if kind == "directory":
        directory = tmp_path / "T001"
        directory.mkdir()
        task = directory / "task.yaml"
        value = directory.name
    else:
        task = tmp_path / f"T001.{kind}"
        value = task.name
    task.write_text("task_id: T001\n", encoding="utf-8")
    assert entrypoints.run("runner", value) == (
        str(task.resolve()),
        str(task.parent.resolve()),
    )
