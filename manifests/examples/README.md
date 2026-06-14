Example model manifests for Aileron packagers and developers.

These files are intentionally kept under `manifests/examples/` so they are not
loaded by Aileron's default catalog discovery. To try one locally, copy it into
`manifests/models/` and make sure a matching runtime manifest exists under
`manifests/runtimes/`.

The Hugging Face artifact URLs, SHA-256 values, and byte sizes were taken from
the public Hugging Face model API with blob metadata enabled. Aileron derives the
user-facing install size from `artifacts[].size_bytes`, so size metadata stays
next to the exact URL and hash it describes.

Artifact `role` values document how the runtime consumes each file. The vision
example uses both `model` and `mmproj` because llama.cpp-style image models need
a projector file in addition to the main GGUF model.
