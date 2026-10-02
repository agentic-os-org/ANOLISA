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

"""Dataset loading and filtering utilities."""

from __future__ import annotations

import logging
import re

import datasets as hf_datasets

from swe_runner.common.dataset_registry import DATASET_MAPPING, get_dataset_name
from swe_runner.common.models import DatasetConfig, SWEInstance

logger = logging.getLogger(__name__)


def load_dataset(config: DatasetConfig) -> list[SWEInstance]:
    path = get_dataset_name(config.subset) if config.subset in DATASET_MAPPING else config.subset
    logger.info(
        "DATASET_LOAD_START instance=global subset=%s resolved_path=%s split=%s",
        config.subset,
        path,
        config.split,
    )
    ds = hf_datasets.load_dataset(path, split=config.split)
    instances = [SWEInstance.from_dataset_row(row) for row in ds]
    logger.info("DATASET_LOAD_DONE instance=global raw_instances=%s path=%s", len(instances), path)
    filtered = filter_instances(instances, config)
    logger.info(
        "DATASET_FILTER_DONE instance=global filtered_instances=%s raw_instances=%s",
        len(filtered),
        len(instances),
    )
    return filtered


def filter_instances(instances: list[SWEInstance], config: DatasetConfig) -> list[SWEInstance]:
    """Apply filters to a list of SWEInstance objects.

    Filters are applied in order:
    1. instance_ids — keep only matching IDs
    2. filter_regex — keep only IDs matching the regex
    3. slice_range — slice the result list

    Args:
        instances: Unfiltered list of instances.
        config: Dataset configuration with filter settings.

    Returns:
        Filtered list of instances.
    """
    result = list(instances)
    logger.info(
        "FILTER_START instance=global total=%s instance_ids=%s filter_regex=%s slice_range=%s",
        len(result),
        len(config.instance_ids) if config.instance_ids else 0,
        config.filter_regex,
        config.slice_range,
    )

    if config.instance_ids:
        id_set = set(config.instance_ids)
        result = [i for i in result if i.instance_id in id_set]
        logger.info("FILTER_INSTANCE_IDS instance=global remaining=%s requested=%s", len(result), len(id_set))

    if config.filter_regex:
        pattern = re.compile(config.filter_regex)
        result = [i for i in result if pattern.search(i.instance_id)]
        logger.info("FILTER_REGEX instance=global remaining=%s pattern=%s", len(result), config.filter_regex)

    slice_tuple = config.get_slice()
    if slice_tuple is not None:
        start, end = slice_tuple
        result = result[start:] if end == -1 else result[start:end]
        logger.info("FILTER_SLICE instance=global remaining=%s start=%s end=%s", len(result), start, end)

    logger.info("FILTER_END instance=global remaining=%s", len(result))
    return result
