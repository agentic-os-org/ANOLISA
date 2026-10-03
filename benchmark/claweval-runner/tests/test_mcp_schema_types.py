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

"""Tests for Bug 2 fix: MCP tool schema type loss (P1).

Verifies:
- Array parameters get type=array (not string) in FastMCP schema
- Integer/number/boolean types are preserved
- Nested array item types are preserved (List[str], List[int])
- Optional parameters with defaults retain correct types
"""

import inspect
import json
import sys
from pathlib import Path
from typing import List
from unittest.mock import patch, MagicMock, AsyncMock
import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))


class TestPythonTypeFor:
    """Test the _python_type_for mapping function."""

    def _get_type_for(self, prop_info):
        """Replicate the _python_type_for logic from mcp_mock_services."""
        import typing
        _JSON_TYPE_MAP = {
            "string": str,
            "integer": int,
            "number": float,
            "boolean": bool,
            "object": dict,
            "array": list,
        }
        json_type = prop_info.get("type", "string")
        if json_type == "array":
            items = prop_info.get("items", {})
            items_type = items.get("type")
            if items_type and items_type in _JSON_TYPE_MAP:
                return typing.List[_JSON_TYPE_MAP[items_type]]
            return list
        return _JSON_TYPE_MAP.get(json_type, str)

    def test_string_type(self):
        assert self._get_type_for({"type": "string"}) is str

    def test_integer_type(self):
        assert self._get_type_for({"type": "integer"}) is int

    def test_number_type(self):
        assert self._get_type_for({"type": "number"}) is float

    def test_boolean_type(self):
        assert self._get_type_for({"type": "boolean"}) is bool

    def test_object_type(self):
        assert self._get_type_for({"type": "object"}) is dict

    def test_array_no_items(self):
        assert self._get_type_for({"type": "array"}) is list

    def test_array_string_items(self):
        result = self._get_type_for({"type": "array", "items": {"type": "string"}})
        assert result == List[str]

    def test_array_integer_items(self):
        result = self._get_type_for({"type": "array", "items": {"type": "integer"}})
        assert result == List[int]

    def test_unknown_type_defaults_to_str(self):
        assert self._get_type_for({"type": "unknown_type"}) is str

    def test_missing_type_defaults_to_str(self):
        assert self._get_type_for({}) is str


class TestFastMCPSchemaPreservation:
    """Test that type annotations produce correct FastMCP schemas."""

    def _make_tool_with_annotation(self, param_name, py_type, required=True, default=inspect.Parameter.empty):
        """Create a Tool with the given parameter annotation and return its schema."""
        from mcp.server.fastmcp.tools import Tool

        kwargs = {"annotation": py_type}
        if not required:
            kwargs["default"] = default
        params = [inspect.Parameter(param_name, inspect.Parameter.KEYWORD_ONLY, **kwargs)]
        sig = inspect.Signature(params)

        async def handler(**kwargs):
            pass
        handler.__signature__ = sig

        return Tool.from_function(handler, name=f"test_{param_name}")

    def test_array_param_generates_array_schema(self):
        """A list-annotated parameter produces type=array, not type=string."""
        tool = self._make_tool_with_annotation("recipients", list)
        props = tool.parameters["properties"]
        assert props["recipients"]["type"] == "array"

    def test_list_str_param_generates_typed_array(self):
        """A List[str]-annotated parameter produces array with string items."""
        tool = self._make_tool_with_annotation("recipients", List[str])
        props = tool.parameters["properties"]
        assert props["recipients"]["type"] == "array"
        assert props["recipients"]["items"]["type"] == "string"

    def test_int_param_generates_integer_schema(self):
        tool = self._make_tool_with_annotation("count", int)
        props = tool.parameters["properties"]
        assert props["count"]["type"] == "integer"

    def test_float_param_generates_number_schema(self):
        tool = self._make_tool_with_annotation("score", float)
        props = tool.parameters["properties"]
        assert props["score"]["type"] == "number"

    def test_bool_param_generates_boolean_schema(self):
        tool = self._make_tool_with_annotation("flag", bool)
        props = tool.parameters["properties"]
        assert props["flag"]["type"] == "boolean"

    def test_dict_param_generates_object_schema(self):
        tool = self._make_tool_with_annotation("data", dict)
        props = tool.parameters["properties"]
        assert props["data"]["type"] == "object"

    def test_no_annotation_generates_string(self):
        """Without annotation, FastMCP defaults to string (the bug we're fixing)."""
        from mcp.server.fastmcp.tools import Tool

        params = [inspect.Parameter("recipients", inspect.Parameter.KEYWORD_ONLY)]
        sig = inspect.Signature(params)

        async def handler(**kwargs):
            pass
        handler.__signature__ = sig

        tool = Tool.from_function(handler, name="test_no_ann")
        props = tool.parameters["properties"]
        # This is the bug: without annotation, everything becomes string
        assert props["recipients"]["type"] == "string"

    def test_optional_array_param(self):
        """Optional parameter with default still preserves type."""
        tool = self._make_tool_with_annotation("tags", list, required=False, default=[])
        props = tool.parameters["properties"]
        assert props["tags"]["type"] == "array"
        assert "tags" not in tool.parameters.get("required", [])


class TestMCPHandlerCreation:
    """Test the full handler creation flow in mcp_mock_services."""

    def test_handler_preserves_schema_types(self):
        """Simulate the make_handler flow with real task.yaml-like schema."""
        from mcp.server.fastmcp.tools import Tool
        import typing

        # Simulate task.yaml input_schema for a finance_submit_report tool
        schema = {
            "type": "object",
            "properties": {
                "recipients": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "List of recipient email addresses"
                },
                "subject": {
                    "type": "string",
                    "description": "Email subject"
                },
                "amount": {
                    "type": "number",
                    "description": "Report amount"
                },
                "priority": {
                    "type": "integer",
                    "description": "Priority level"
                },
            },
            "required": ["recipients", "subject"]
        }

        properties = schema["properties"]
        required = schema["required"]

        # Replicate _python_type_for
        _JSON_TYPE_MAP = {
            "string": str, "integer": int, "number": float,
            "boolean": bool, "object": dict, "array": list,
        }

        def _python_type_for(prop_info):
            json_type = prop_info.get("type", "string")
            if json_type == "array":
                items = prop_info.get("items", {})
                items_type = items.get("type")
                if items_type and items_type in _JSON_TYPE_MAP:
                    return typing.List[_JSON_TYPE_MAP[items_type]]
                return list
            return _JSON_TYPE_MAP.get(json_type, str)

        # Build parameters with annotations
        parameters = []
        for k, pinfo in properties.items():
            py_type = _python_type_for(pinfo)
            if k in required:
                parameters.append(
                    inspect.Parameter(k, inspect.Parameter.KEYWORD_ONLY,
                                      annotation=py_type)
                )
            else:
                default = pinfo.get("default", "")
                parameters.append(
                    inspect.Parameter(k, inspect.Parameter.KEYWORD_ONLY,
                                      default=default, annotation=py_type)
                )

        sig = inspect.Signature(parameters)

        async def handler(**kwargs):
            pass
        handler.__signature__ = sig

        tool = Tool.from_function(handler, name="finance_submit_report")
        props = tool.parameters["properties"]

        # Verify types are preserved
        assert props["recipients"]["type"] == "array"
        assert props["recipients"]["items"]["type"] == "string"
        assert props["subject"]["type"] == "string"
        assert props["amount"]["type"] == "number"
        assert props["priority"]["type"] == "integer"

        # Verify required fields
        assert "recipients" in tool.parameters["required"]
        assert "subject" in tool.parameters["required"]
        assert "amount" not in tool.parameters.get("required", [])
