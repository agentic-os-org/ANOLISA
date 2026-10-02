# Parameter Guide

Validate parameter shape before running remote diagnosis. Avoid collecting
credentials in the conversation.

## Global Parameters

| Parameter | Required | Description | Example |
|-----------|----------|-------------|---------|
| `--region` | Remote cross-instance | Alibaba Cloud region ID | `cn-hangzhou` |
| `--instance` | Remote cross-instance | ECS instance ID | `i-bp1cg751dz3lssnbd14a` |

When running on the target ECS instance, sysom-osops can auto-detect both values
from ECS metadata. For diagnosing another instance, use both flags together.

## Mode Summary

| Mode | Description | Credential required |
|------|-------------|---------------------|
| Local memory classify | Reads local Linux state and returns an Agent envelope | No |
| Remote self diagnosis | Diagnoses the current ECS through SysOM backend | Yes |
| Remote cross-instance diagnosis | Diagnoses a different ECS by explicit region and instance | Yes |

## Memory Parameters

| Command | Parameter | Description |
|---------|-----------|-------------|
| `memory oom` | `--oom-at` | Select an OOM event near a user-provided timestamp |
| `memory oom` | `--time-start`, `--time-end` | Limit OOM event search to a user-provided window |
| `memory oom` | `--select`, `--event-id`, `--index` | Pivot to a different event when the user clearly identifies one |
| `memory filecache` | `--top` | Limit number of cached files in the report |
| `memory filecache` | `--sample-rate` | Adjust sampling rate when a full sample is too expensive |
| `memory javamem` | `--pod` | Scope Java analysis to a pod when the user provides it |
| `memory memgraph` | `--pod` | Scope memory landscape to a pod when the user provides it |
| `memory memgraph` | `--verbose` | Request a broader memory breakdown when needed |
| `memory memgraph` | `--enable-socket` | Include socket buffer and socket holder attribution when socket pressure is visible |
| `memory memcgoffline` | `--max-files`, `--max-items` | Bound offline-cgroup scan output size |
| `memory memcgoffline` | `--cgroup-path` | Narrow analysis to a cgroup already named by SysOM output |
| `memory memleak` | `--type` | Choose leak class `slab`/`page`/`vmalloc`/`percpu` (default vmalloc) per the kernel-hidden evidence |

## IO, Load, and Network Parameters

| Domain | Common parameters | Description |
|--------|-------------------|-------------|
| IO | `--timeout`, `--disk` | Bound collection time or scope to a disk named by the user or prior output |
| Load | `--duration`, `--threshold` | Bound collection time or use a user-provided sensitivity threshold |
| Network | `--duration`, `--threshold` | Bound packet-drop or jitter monitoring when the user provides a window or threshold |

Use optional parameters only to honor user-provided scope or to fill a missing
entity identified by the envelope.
