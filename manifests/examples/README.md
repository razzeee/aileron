The shipped model catalog lives in `manifests/models/` as editable JSON files
for packagers and distributions. Each shipped catalog manifest should include an
`llmfit_model_id` that resolves in `llmfit-core` so the UI can show fit data.

Additional examples can be kept here when they are useful for local testing, but
files under this directory are not loaded by Aileron's default catalog discovery.
