# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

from __future__ import annotations

from pathlib import Path

from swe_runner.common.models import SWEInstance
from swe_runner.run.workspace.docker_refs import (
    build_pull_image_name,
    default_work_dir,
    default_workspace_root,
    get_docker_image_name,
    image_has_registry,
    safe_docker_name,
)


def _instance(**overrides: object) -> SWEInstance:
    values = {
        "instance_id": "django__django-1234",
        "repo": "django/django",
        "version": "3.2",
        "base_commit": "abc123",
        "problem_statement": "Fix it",
        "patch": "",
        "test_patch": "",
    }
    values.update(overrides)
    return SWEInstance(**values)


def test_safe_docker_name_sanitizes_and_prefixes_leading_non_alnum() -> None:
    assert safe_docker_name("django__django-1234") == "django__django-1234"
    assert safe_docker_name("foo bar/baz") == "foo-bar-baz"
    assert safe_docker_name("repo@sha256:abc") == "repo-sha256-abc"
    assert safe_docker_name("__leading") == "s__leading"


def test_default_workspace_paths_are_instance_scoped() -> None:
    assert default_work_dir("django__django-1234") == Path("/tmp/swebench_work_django__django-1234")
    assert default_work_dir("repo/name with space") == Path("/tmp/swebench_work_repo_name_with_space")
    assert default_workspace_root("repo/name with space") == default_work_dir("repo/name with space")


def test_get_docker_image_name_prefers_explicit_fields() -> None:
    assert get_docker_image_name(_instance()) == "swebench/sweb.eval.x86_64.django_1776_django-1234:latest"
    assert get_docker_image_name(_instance(docker_image="registry/docker:1")) == "registry/docker:1"
    instance_with_both_images = _instance(
        image_name="registry/image:2",
        docker_image="registry/docker:1",
    )
    assert get_docker_image_name(instance_with_both_images) == "registry/image:2"


def test_build_pull_image_name_applies_registry_without_duplicating_existing_registry() -> None:
    assert image_has_registry("localhost/repo:tag") is True
    assert image_has_registry("example.com/repo:tag") is True
    assert image_has_registry("swebench/repo:tag") is False
    assert build_pull_image_name("swebench/repo:tag", None) == "swebench/repo:tag"
    assert build_pull_image_name("swebench/repo:tag", "mirror.local") == "mirror.local/swebench/repo:tag"
    assert build_pull_image_name("example.com/swebench/repo:tag", "mirror.local/") == "mirror.local/swebench/repo:tag"
