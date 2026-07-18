# Stub Runtime

This runtime implements the Aileron container stdio protocol without loading a model. It is intended for end-to-end tests of the daemon, portal, and clients.

The stub supports the container stdio request types with deterministic fake responses, including `generate`, `generate_structured`, `embed`, `transcribe`, `describe`, `ocr`, `detect`, `segment`, and `depth`.

Run all commands below from the repository root.

## Runtime ID

```json
{
  "runtime_id": "stub"
}
```

## Build

```sh
podman build -f runtimes/stub/Dockerfile -t docker.io/example/aileron-runtime-stub:cpu .
```

The project also publishes this runtime to GHCR from the runtime image workflow:

```sh
podman pull ghcr.io/<owner>/aileron-runtime-stub:cpu
```

## Runtime Manifest

Publish the image ref through a runtime manifest file such as `/usr/share/aileron/manifests/runtimes/stub.json`:

```json
{
  "runtime_id": "stub",
  "images": {
    "cpu": "ghcr.io/<owner>/aileron-runtime-stub:cpu"
  }
}
```

## Model Profile

The stub does not need real artifacts. Install it through a manifest with an empty `artifacts` list; see the root README end-to-end test.
