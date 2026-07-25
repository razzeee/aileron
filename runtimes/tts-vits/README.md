# tts-vits

Offline CPU runtime for generated llmfit VITS profiles. Aileron mounts a verified, flat Transformers snapshot read-only at `/model`; the image contains no model weights and uses local files only. Pinned uroman and phonemizer dependencies plus the espeak-ng backend cover the text-normalization modes used by the supported VITS catalog.

The runtime accepts newline-delimited `synthesize` requests, supports the model's default voice through an empty `voice_id`, emits mono `s16le` PCM in 50 ms chunks, and terminates each request with an empty `done=true` event. `SUPPORTED_LANGUAGES` is supplied by the generated profile when llmfit language metadata exists. Language hints accept equivalent ISO 639-1, ISO 639-3, and BCP 47 primary language identifiers.

Run the deterministic unit tests from this directory with:

```sh
PYTHONPATH=. python3 -m unittest discover -s tests
```
