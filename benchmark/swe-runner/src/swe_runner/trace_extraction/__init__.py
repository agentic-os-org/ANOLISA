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

"""Public API for trace extraction and analysis."""

from swe_runner.trace_extraction.analysis import analyze_trace_files
from swe_runner.trace_extraction.export import write_trace_analysis_csvs
from swe_runner.trace_extraction.helpers import ExtractionError
from swe_runner.trace_extraction.plan import TraceCollectionPlan
from swe_runner.trace_extraction.recording import record_openclaw_jsonl_traces_in_window

__all__ = [
    "ExtractionError",
    "TraceCollectionPlan",
    "analyze_trace_files",
    "write_trace_analysis_csvs",
    "record_openclaw_jsonl_traces_in_window",
]
