# Stub Image

This image implements the Aileron container stdio protocol without loading a model. It is intended for end-to-end tests of the daemon, portal, and clients.

The stub supports `generate`, `generate_structured`, `transcribe`, and `describe` requests with deterministic fake responses.

Run all commands below from the repository root.

## Build

```sh
podman build -t aileron/stub:latest images/stub
```

## Assignment

Assign the stub to any use case you want to exercise:

```sh
varlink call "unix:$XDG_RUNTIME_DIR/aileron.socket/aileron.Models.AssignUseCase" \
    '{"image_ref":"localhost/aileron/stub:latest","use_case":"llm.summarize"}'
```

The root README contains a full end-to-end test flow using this image.
