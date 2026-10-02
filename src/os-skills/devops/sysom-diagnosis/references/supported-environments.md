# Supported Environments

## Target Instance OS

| Architecture | Supported distributions |
|--------------|-------------------------|
| x86_64 | Alibaba Cloud Linux 2/3, Alibaba Cloud Linux 3 Pro, Alibaba Cloud Linux 3 Container Optimized Edition, CentOS 7.6+, CentOS 8, Rocky Linux 8.8/9.1/9.5, Ubuntu 20.04/22.04/24.04, Anolis OS 7/8 |
| aarch64 | Alibaba Cloud Linux 3, Alibaba Cloud Linux 3 Pro |

## Region Availability

Remote diagnosis is available in China Mainland regions and China (Hong Kong).

## Prerequisites

- The target ECS instance is Linux.
- Cloud Assistant is online on the target ECS instance.
- Remote commands have valid Alibaba Cloud credentials through AK/SK or ECS RAM Role.
- Java memory diagnosis requires OpenJDK 1.8 or later on the target instance.

## Unsupported Scenarios

- Windows instances.
- Instances without Cloud Assistant online.
- Regions outside China Mainland and Hong Kong.
- Pure configuration questions without a SysOM diagnosis symptom, such as generic
  security group or VPC routing administration.
