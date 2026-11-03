# Images and cloud services

[中文版](../../zh/getting-started/images.md)

ANOLISA is an open-source project initiated by the OpenAnolis community.
The project is developed and maintained under
[agentic-os-org](https://github.com/agentic-os-org) on GitHub.

This catalog lists preinstalled system images, container base images, and
ready-to-use cloud services. Community contributors and vendors are welcome to
add ISO and VM image downloads, container image pull references, and cloud
service entry points.

## Full system images

This section collects full system images, such as ISO and VM images, that you
can download and deploy yourself. No standalone download links are listed yet;
community contributors and vendors are welcome to add them.

## Container base images

Use a container base image as `FROM` when building an Agent runtime or platform
image, and install the component packages needed by that environment.

| Image / maintainer | System / architecture | Preinstalled content | Pull reference and usage |
|---|---|---|---|
| Agentic OS base image / Alibaba Cloud | Alibaba Cloud Linux 4 / linux/amd64 | ANOLISA CLI and component repository configuration | `ac2-registry.cn-hangzhou.cr.aliyuncs.com/ac2/agentic-os:1.3` · [Base image and derived-image guide (Chinese)](https://help.aliyun.com/zh/alinux/agentic-os-base-container-image) |

This base image does not include component binaries. Install the component
packages you need at image build time. The component guides explain runtime
dependencies, and the image guide linked above explains startup behavior.

## Cloud service entry points

You can experience ANOLISA in a preinstalled environment provided by a cloud
service, without downloading a system image yourself.

| Service / provider | Entry point and usage |
|---|---|
| Elastic Compute Service (ECS) / Alibaba Cloud | Follow the [ECS public image and first-use guide (Chinese)](https://help.aliyun.com/zh/alinux/agentic-os-getting-started) to select Alibaba Cloud Linux 4 Agentic Edition when creating an instance or changing its operating system, then complete setup and model authorization. |
| Simple Application Server (SWAS), Agent-dedicated instance / Alibaba Cloud | Open the [instance creation page](https://swasnext.console.aliyun.com/buy) and select **Alibaba Cloud Linux 4 Agentic 版**. |
| Wuying cloud computer (AI PC) / Alibaba Cloud | Open the [image page](https://wuying-personal.aliyun.com/platform#/imageZone?defaultImageCode=93316207) in Wuying Personal Edition and follow the platform's instructions. |

## Contribute an image or cloud service entry

To add or update an entry, open a pull request editing this page and its
Chinese counterpart. Include:

- For images: the name, maintainer, system and architecture, preinstalled
  content, download or pull reference, and usage instructions.
- For cloud services: the service name, provider, entry point, and instructions
  for selecting the required image or instance.

Image maintainers can publish image files through Releases, an image mirror,
or an OCI registry. This catalog collects links and identifies each entry's
maintainer or provider;
the images and cloud services are published and operated by their respective
vendors or community maintainers.

See the [contribution guide](../../../../CONTRIBUTING.md) for the PR workflow.
