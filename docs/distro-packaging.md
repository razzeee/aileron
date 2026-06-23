# Distribution Packaging Guide

This document describes how to package Aileron for traditional Linux distributions.

Aileron is not a container engine frontend. The daemon owns a small OCI runtime store under the user's XDG data directory, fetches runtime images with `skopeo`, renders them to root filesystems, and starts hardened OCI bundles with `crun`.

## Components

Package these binaries from the Rust workspace:

| Binary | Purpose |
|---|---|
| `aileron-daemon` | User service, Varlink API, model/runtime manager, OCI bundle launcher |
| `aileron-portal` | xdg-desktop-portal backend for sandboxed apps |
| `aileron` | GTK/libadwaita management UI |
| `aileron-demo` | Optional demo app |

The daemon and portal are useful without the management UI on headless or minimal installs.

## Build Dependencies

Required for all packages:

- Rust toolchain supporting edition 2024
- `pkg-config`
- D-Bus development headers

Required for GTK packages:

- GTK 4 development headers
- libadwaita development headers

Rust dependencies are managed by Cargo. Distro packagers should vendor or package crates according to their normal Rust packaging policy.

## Runtime Dependencies

Declare these as runtime dependencies for `aileron-daemon`:

- `skopeo`
- `crun`

The daemon invokes `skopeo copy` to fetch OCI runtime images and `crun run` to start generated OCI bundles. Podman is not required.

Recommended runtime dependencies:

- `xdg-desktop-portal` for sandboxed app integration
- `systemd --user` support for service activation

Hardware-specific dependencies are supplied by the host GPU stack and by runtime images. Aileron does not require packagers to depend on CUDA, ROCm, or Vulkan globally.

## Installed Files

Suggested install locations:

| Source | Destination |
|---|---|
| `target/release/aileron-daemon` | `/usr/bin/aileron-daemon` |
| `target/release/aileron-portal` | `/usr/bin/aileron-portal` |
| `target/release/aileron` | `/usr/bin/aileron` |
| `target/release/aileron-demo` | `/usr/bin/aileron-demo` |
| `systemd/aileron-daemon.service` | `/usr/lib/systemd/user/aileron-daemon.service` |
| `systemd/aileron-portal.service` | `/usr/lib/systemd/user/aileron-portal.service` |
| `portal/aileron.portal` | `/usr/share/xdg-desktop-portal/portals/aileron.portal` |
| `portal/org.freedesktop.impl.portal.desktop.aileron.service` | `/usr/share/dbus-1/services/org.freedesktop.impl.portal.desktop.aileron.service` |
| `manifests/` | `/usr/share/aileron/manifests/` |
| packaged runtime rootfs trees | `/usr/lib/aileron/oci/rootfs/<store-key>/` |
| packaged runtime metadata | `/usr/lib/aileron/oci/metadata/<store-key>.json` |
| packaged model artifacts | `/usr/lib/aileron/models/<model-id>/` |

## Shipping Manifests, Runtimes, And Models

Distributions should normally ship manifests. They may also ship read-only runtime rootfs trees and model weights for offline systems.

Recommended package split:

| Package | Contents | Notes |
|---|---|---|
| `aileron-daemon` | daemon binary, user service, Varlink support files | Depends on `skopeo` and `crun` |
| `aileron-portal` | portal backend, D-Bus service file, portal descriptor | Depends on `xdg-desktop-portal` integration according to distro policy |
| `aileron` | management UI | Optional on minimal/headless systems |
| `aileron-manifests` | curated model and runtime manifests under `/usr/share/aileron/manifests/` | Recommended default catalog package |
| `aileron-runtimes-*` | optional runtime manifests or pre-rendered rootfs trees | Use this when the distro wants offline runtime availability or curated image refs |
| `aileron-models-*` | optional model manifests and model weights when license/size policy allows | Keep large or restricted models opt-in |

Runtime manifests are small JSON files that map runtime IDs to OCI image refs. A distro can ship upstream runtime manifests as-is, replace image refs with distro-hosted registry refs, or split hardware families into separate packages. For example, a conservative base package can include only CPU runtime manifests, while separate packages add CUDA, ROCm, or Vulkan runtime manifests.

Model catalog manifests are also small JSON files. They describe downloadable artifacts, checksums, sizes, use-cases, and runtime IDs. Shipping a model manifest does not install the model weights. It only makes the profile visible in the management UI and available to `InstallManifest`; the user still chooses to download it.

If a distro chooses to ship model weights directly, install them under `/usr/lib/aileron/models/<model-id>/` only when the model license, redistribution terms, package size, and update policy are acceptable. Each packaged filename must match the corresponding model manifest artifact `filename` field, and the manifest should be installed under `/usr/share/aileron/manifests/models/` or provided by a dependency.

Do not pre-populate `$XDG_DATA_HOME/aileron/oci/` or `$XDG_DATA_HOME/aileron/models/` from a system package. Those directories are per-user daemon state and may contain user-selected profiles, rendered root filesystems, runtime metadata, permissions, and assignments.

## Offline Deployment

Aileron resolves artifacts from user-managed paths first, then distro-managed system paths, then falls back to download when a user explicitly installs or updates online content.

Runtime rootfs lookup order:

```text
$XDG_DATA_HOME/aileron/oci/rootfs/<store-key>/
/usr/lib/aileron/oci/rootfs/<store-key>/
```

Model artifact lookup order:

```text
$XDG_DATA_HOME/aileron/models/<model-id>/
/usr/lib/aileron/models/<model-id>/
```

`<store-key>` is derived from the image reference by keeping ASCII letters, digits, `.`, `-`, and `_`, and replacing every other character with `_`. For example, `ghcr.io/razzeee/aileron-runtime-llm-vision-whisper:cpu` becomes `ghcr.io_razzeee_aileron-runtime-llm-vision-whisper_cpu`.

Runtime packages for offline use should install pre-rendered rootfs trees and optional metadata:

```text
/usr/lib/aileron/oci/rootfs/<store-key>/
/usr/lib/aileron/oci/metadata/<store-key>.json
```

Model packages should install artifacts under:

```text
/usr/lib/aileron/models/<model-id>/
```

The daemon verifies packaged model artifacts against manifest SHA-256 values before exposing the system-backed profile. Incomplete or corrupted system model directories are ignored.

System artifacts are read-only. Users can shadow a system profile or runtime by installing a user-managed copy with the same profile ID, model ID, or runtime store key. Deleting the user-managed copy reveals the packaged system copy again if it is still installed. The management UI labels artifacts as `System` or `User`; system-backed profiles and runtime images cannot be removed through Aileron.

Traditional distro packages, live/install media, immutable OS images, rpm-ostree layers, NixOS configurations, transactional snapshots, and systemd-sysext extensions can all provide `/usr/lib/aileron` without creating per-user home directories or running per-user seeding scriptlets.

## User Data

By default, the daemon stores mutable data under `$XDG_DATA_HOME/aileron`, usually `~/.local/share/aileron`.

Important subdirectories:

| Path | Owner | Description |
|---|---|---|
| `$XDG_DATA_HOME/aileron/models/` | daemon | Downloaded model artifacts |
| `$XDG_DATA_HOME/aileron/oci/` | daemon | OCI layouts, rendered rootfs trees, metadata, and generated bundles |
| `$XDG_DATA_HOME/aileron/manifests/` | user/admin | Optional user-provided manifests |

System manifests are discovered from `/usr/share/aileron/manifests` and `/etc/aileron/manifests`. Users can add manifests without modifying packaged files.

## Services

Enable these user services according to distro policy:

- `aileron-daemon.service`
- `aileron-portal.service`

The daemon listens on `$XDG_RUNTIME_DIR/aileron.socket`. The portal service connects to that socket and exposes the portal backend over D-Bus.

Do not run `aileron-daemon` as a system service. It manages per-user permissions, per-user model artifacts, per-user runtime images, and per-user application sessions.

## OCI Runtime Execution

Aileron generates OCI bundles directly and starts them with `crun`.

The generated bundles:

- mount the selected model artifact directory at `/model` read-only
- run the runtime entrypoint with stdin/stdout JSON protocol
- do not expose a network service
- configure PID, IPC, UTS, mount, network, and cgroup namespaces for `crun`
- drop Linux capabilities
- set `noNewPrivileges`
- set memory and PID limits

The daemon renders OCI layouts to rootfs directories with the `ocirender` Rust crate. It does not require Podman, Docker, containerd, or a host container image store.

## Hardware Access

Runtime manifests map each runtime ID to image variants such as `cpu`, `cuda`, `rocm`, and `vulkan`. The daemon chooses the best variant detected on the host, falls back from CUDA or ROCm to Vulkan when available, and uses CPU as the final fallback. Intel GPUs use the existing `vulkan` variant rather than a separate `intel` variant. The combined ML runtime also retries cold start with reduced `N_GPU_LAYERS` values before moving to the next image candidate, unless the profile explicitly sets `N_GPU_LAYERS`.

The generated OCI bundle exposes only the hardware needed by the selected variant:

| Variant | Bundle access |
|---|---|
| `cpu` | no accelerator device mounts, no accelerator topology mount |
| `vulkan` | `/dev/dri`, `/sys`, `/dev/shm` |
| `rocm` | `/dev/kfd`, `/dev/dri`, `/sys`, `/dev/shm` |
| `cuda` | `/sys`, existing `/dev/nvidia*`, optional `/proc/driver/nvidia`, discovered NVIDIA driver libraries, `/dev/shm` |

Actual device bind mounts must not use `nodev`; GPU runtimes need functional device nodes, not only visible paths. Non-device mounts such as `/tmp`, `/dev/shm`, and `/sys` keep restrictive flags where possible.

Distro packages should not add broad device permissions themselves. Device access should follow the distro's normal GPU stack policy, udev rules, and user group configuration.

Intel Vulkan acceleration requires render nodes such as `/dev/dri/renderD*` and an Intel Vulkan ICD. Common distro packages include Mesa's Intel Vulkan driver, often named `mesa-vulkan-drivers`, `mesa-vulkan-intel`, or `vulkan-intel`, plus `vulkan-tools` if administrators want to validate with `vulkaninfo`.

CUDA runtimes require a working host NVIDIA kernel driver and driver userspace libraries such as `libcuda.so.1`. Aileron discovers these libraries through `ldconfig -p` and common system library paths, then mounts them read-only into CUDA bundles under `/usr/local/nvidia/lib64`.

## Network And Downloads

Aileron downloads two kinds of data at user request:

- model artifacts declared in model manifests
- OCI runtime images declared in runtime manifests

Each model artifact manifest includes a URL, SHA-256, filename, and size. The daemon verifies model artifact hashes after download.

Runtime images are fetched with `skopeo` from OCI registries. Distro policies differ on applications downloading executable runtime images at first use. If this is not acceptable for your distribution, package alternative runtime manifests pointing at distro-hosted or locally mirrored OCI images.
