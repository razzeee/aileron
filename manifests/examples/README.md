Example model manifests for Aileron packagers and developers.

These files are intentionally kept under `manifests/examples/` so they are not
loaded by Aileron's default catalog discovery. To try one locally, copy it into
`manifests/models/` and make sure a matching runtime manifest exists under
`manifests/runtimes/`.

The Hugging Face artifact URLs, SHA-256 values, and byte sizes were taken from
the public Hugging Face model API with blob metadata enabled. Aileron can parse
compact single-file manifests with `artifact`, or explicit/multi-file manifests
with `artifacts`.

Disk-loaded compact manifests can omit `profile_id` and `model_id`; Aileron uses
the JSON filename stem for both. They can also omit `runtime_id`, `use_cases`,
`role`, and `filename` when llmfit metadata plus the artifact extension are
enough to infer safe defaults. Explicit manifests should still provide fields
that cannot be inferred safely, such as speech runtimes, vision runtimes, runtime
options, multi-artifact roles, or intentionally restricted use-cases.

Aileron derives the user-facing install size from `artifacts[].size_bytes` when
present, so size metadata stays next to the exact URL and hash it describes.

Artifact `role` values document how the runtime consumes each file. The vision
example uses both `model` and `mmproj` because llama.cpp-style image models need
a projector file in addition to the main GGUF model.
