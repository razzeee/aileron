# Stub Runtime

This runtime implements the Aileron container stdio protocol without loading a model. It is intended for end-to-end tests of the daemon, portal, and clients.

The stub supports `generate`, `generate_structured`, `transcribe`, and `describe` requests with deterministic fake responses.

Run all commands below from the repository root.

## Runtime ID

```json
{
  "runtime_id": "stub"
}
```

## Build

```sh
podman build -t docker.io/example/aileron-runtime-stub:cpu runtimes/stub
```

## Runtime Manifest

Publish the image ref through a runtime manifest file such as `/usr/share/aileron/manifests/runtimes/stub.json`:

```json
{
  "runtime_id": "stub",
  "images": {
    "cpu": "docker.io/example/aileron-runtime-stub:cpu"
  }
}
```

## Model Profile

The stub does not need real artifacts. Install it through a manifest with an empty `artifacts` list; see the root README end-to-end test.
