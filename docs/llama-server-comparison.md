# Full-replacement comparison

Status: the production cutover and final CPU/Vulkan integration checks are
implemented. All four accelerator images build. The user accepted the measured
CPU startup tradeoff. Hardware coverage is tracked separately; this is not a
claim that every original gate passed.

The preserved native reference is the local image
`46a4b1b3ce4aae9b5ee400d93a88ae0d63b85b45b408eae2e9ef7b5736eedc1a`,
tagged `localhost/aileron-native-reference:replacement-baseline` and exported
to `target/aileron-native-reference.tar`. It contains the existing native
text/vision executables built with llama-cpp-2 0.1.154, whose bundled llama.cpp
revision is `5f55650a78f92aff4d48d671423e888fac0469ff`.

The current candidate server revision is
`5f55650a78f92aff4d48d671423e888fac0469ff`, matching the native engine.
The original candidate used `e6ab7c1a41054a888ada952eab4c886444c2f5ad`;
its measurements below include both engine and adapter differences.

`runtimes/llm-vision-whisper/bench/compare.py` records image inspection metadata,
model hashes, configuration, per-request events, latency and available host
resource measurements. It does not substitute chunk counts for missing token
usage. Raw results are written outside source under `target/`.

The final report must include all workloads and the approved gates in
[the replacement design](llama-server-replacement-design.md). Initial CPU text
results do not establish vision, embeddings, tools, reasoning, or GPU parity.

## Final cutover and graph allocation

The production entrypoint now selects the server for all GGUF profiles. Native
llama executables, bindings, and backend selection are removed; Whisper remains.
The benchmark now exercises the candidate image's normal entrypoint.

Vulkan testing found that reserving a maximum-sized graph on every pooling-mode
switch both stalled requests and retained oversized buffers. The pooling patch
now synchronizes and invalidates the previous graph; normal graph allocation
handles the next actual input. A fourth patch bounds initial graph reservation
to 512 tokens while preserving full graph-node capacity and the configured
input limit. Larger actual requests still grow buffers. CPU and Vulkan tests
with long embeddings and generation interleaved match the pre-optimization
server at cosine `1.0`.

Final full comparisons use five process-cold starts and twenty warm requests:

| Workload | Median change | p95 change | Semantic checks |
| --- | --- | --- | --- |
| CPU Gemma OCR | +3.9% | +3.3% | 25/25 pass |
| CPU dedicated encoder, four fixtures | -2.8% to +4.4% | -1.4% to +2.3% | 25/25 each, cosine `1.000000000` |
| Vulkan Llama generation before embedding | -35.4% | -33.6% | 25/25 pass |
| Vulkan Llama generation after embedding | -22.1% | -20.8% | 25/25 pass |
| Vulkan Llama first embedding | -33.7% | -32.2% | 25/25 pass |
| Vulkan Llama repeated embedding | -33.9% | -33.2% | 25/25 pass |
| Vulkan Gemma OCR | -19.0% | -17.5% | 25/25 pass |

Vulkan hardware was an AMD Radeon RX 5600 XT, RADV NAVI10, 6 GiB VRAM. Mixed
workload peak VRAM was 1.073 GB for both runtimes; OCR used 4.202 GB versus
native's 4.158 GB. Peak sampled PSS increased by less than 10 MiB in these final
comparisons. Cgroup peaks were higher for OCR: CPU 5.094 GB versus 4.078 GB,
Vulkan 1.804 GB versus 0.540 GB. These are retained alongside PSS and RSS rather
than treating the measurements as interchangeable or claiming every memory
metric meets the original budget. A follow-up memory breakdown found almost
identical anonymous-memory peaks: CPU 4.046 GB native / 4.039 GB server;
Vulkan 192.45 MB / 192.24 MB. File-cache charges varied sharply: CPU 4.310 GB
native / 0.394 GB server in that probe, reversing the earlier total-memory
ordering. This supports file-cache charging as the source of inconsistent
cgroup totals, rather than an anonymous-memory growth regression. The diagnostic
reports are `memory-breakdown-cpu.json` and `memory-breakdown-vulkan.json`.

Cold readiness is still slower: CPU OCR +362 ms; Vulkan mixed +262 ms; Vulkan
OCR +924 ms. The original cold-start gate fails for these runs. CPU startup
was accepted by the user; the additional Vulkan OCR startup result is a newly
measured tradeoff and is not silently classified as a pass.

Final CPU image: `localhost/aileron-llama-server:final-cpu`,
`sha256:b321bae06265095125ce404c458b8226215721c6dc0bf236e4cc0982e51a8266`.
Final Vulkan image: `localhost/aileron-llama-server:bounded-graph-vulkan`,
`sha256:f5a4736951140f6b7f510505f0e519a3696e6d83d7518dc104fef1d9372314f6`.
CUDA image: `localhost/aileron-llama-server:final-cuda`,
`sha256:1a1cd46542237011ef25c09155e6a650890cd91657363e4a4191643361112091`.
ROCm image: `localhost/aileron-llama-server:final-rocm`,
`sha256:5954f63a8509c00d055db10e50edd7831661e241d46dd1587c964e86c414ea96`.
Raw evidence is `final-cpu-ocr`, `bounded-graph-vulkan-full`, and
`final-vulkan-ocr`, each with `.json` and `.md` under `target/agent-tmp/`.

CUDA and ROCm required packaging fixes, now verified by complete image builds:
portable CUDA architecture defaults preserve Blackwell's architecture-specific
FP4 targets, and ROCm's static HIP objects require a non-PIE executable link.
CUDA inference remains untested without NVIDIA hardware. ROCm passes mixed
generation/embedding and Qwen3 reasoning on the RX 5600 XT, but OCR fails because
the vendor rocBLAS package lacks `gfx1010` Tensile kernels. This is a recorded
hardware-support limitation, not a passing ROCm vision result.

The old generation-only diagnostic option was removed: its environment selector
did not survive the adapter's environment filtering. Its historical reports
are excluded from acceptance evidence.

## Revised engine pin

The user approved changing the engine pin after the original candidate failed
OCR latency. The Dockerfile now selects the native reference revision. All
three server patches apply and compile on this revision.

Compatibility changes explicitly enable Jinja and use the common
`--no-webui` flag. Older `/props` responses omit
`supports_reasoning_effort`; the adapter recognizes the known GPT-OSS Harmony
template and its effort variable when that field is absent. An explicit
negative capability still takes precedence. Model identity lookup now runs
alongside server startup rather than adding a measured 77 ms serial scan for
the Gemma checkpoint.

| Check | Latest result on revised engine |
| --- | --- |
| Gemma OCR, 5 cold starts / 20 warm requests | 25/25 correct; median +1.7%, p95 +1.5% |
| Llama short answer, 5 cold starts / 20 warm requests | 25/25 correct; median +1.7%, p95 +2.4% |
| Llama mixed generation/embedding | All checks pass; warm medians 1.9% to 5.2% faster |
| Embedding cosine agreement | `1.000000000` for both mixed fixtures |
| Gemma startup-only, 5 alternating starts after parallel identity lookup | Native 3.580 s; candidate 3.636 s; +56 ms, pass |
| Gemma startup in the subsequent combined OCR run | Native 3.588 s; candidate 4.072 s; +483 ms exceeds the 359 ms allowance |
| Llama text startup | Native 0.979 s; candidate 1.127 s; +149 ms, pass |
| GPT-OSS | Low/medium/high, trace privacy, invalid settings all pass |
| DeepSeek R1 | Thinking-only behavior and invalid settings pass |
| Real daemon with Gemma | Tools, schema continuation, replay/session isolation, thinking on/off, trace privacy, exhaustion, cancellation and reuse pass |

Earlier revised-engine runs encountered unrelated host CPU and memory load.
Their cold readiness ranged from 5 to 20 seconds and is retained as evidence,
not discarded into a passing aggregate. After load fell, the full OCR recheck
passed warm latency but still failed its startup threshold. The subsequent
startup optimization was verified separately with five alternating starts.
The subsequent combined run passes OCR latency and memory but fails startup.
The isolated startup result does not override that failure. The user accepted
the measured startup regression after reviewing the combined result. Retain
the failed gate in the report as an accepted tradeoff rather than changing its
threshold. Accelerator and final integration checks remain before cutover.

The latest built image is `localhost/aileron-llama-server:reference-revision-candidate`,
ID `2e1da15be470d3f4bbe428dc47c9e7cfa958f00510d06df80a869eae1e825bd3`,
digest `sha256:2719ba024c0b4613a205e7dd733c7aa11600f2b86111883350a5c08f7d8c05a9`.
Each report retains its own exact image digest; conformance reports and the
earlier OCR recheck precede the final startup optimization. The final combined
OCR run uses the latest image. The `replacement` image tag and
previous OCI exports must not be assumed to contain this latest image.

Evidence under `target/agent-tmp/`:

- `reference-revision-ocr-recheck.{json,md}`
- `reference-revision-final-ocr.{json,md}`
- `reference-revision-text-full.{json,md}`
- `reference-revision-mixed-full.{json,md}`
- `reference-revision-parallel-startup.{json,md}`
- `reference-revision-gpt-oss-fixed.json`
- `reference-revision-thinking-only.json`
- `reference-revision-daemon.json`
- `reference-revision-gpt-oss-props.json`

Runtime tests, Clippy with warnings denied, formatting, and the seven Python
runtime/benchmark tests pass after these changes. At that measurement stage
the production backend switch and native-code removal had not yet happened;
the final cutover above supersedes that status.

## CPU measurements, 25 September 2026

Candidate image `9cfb24f77a3916d794472d4752f98475763ce34fc576bcd220048cafe16f7e0f`
has digest `sha256:06d911b31b143e87c85056a321e4add37e79290f4bf835d51e324c21fcf9a3dd`.
Each comparison used five process-cold starts and twenty warm requests per
workload, two inference threads, and a 4096-token context. Accelerator build
processes were paused during each comparison and resumed afterward. These
are process-cold measurements, not measurements with a flushed OS page cache.

Two changes addressed earlier failures:

- Plain, single-turn Llama-3 generation uses the upstream legacy template
  renderer to preserve native prompt behavior. Tools, images, explicit
  reasoning, and richer conversation histories retain Jinja rendering.
  The mixed fixture now produces the requested goodbye after an embedding.
- Automatic server memory fitting is disabled. Aileron supplies context and
  GPU-layer settings and owns fallback policy. The server's fitting pass
  added approximately 350 ms of duplicate inspection in the startup probe.

| Model and workload | Warm median change | Warm p95 change | Outcome |
| --- | --- | --- | --- |
| APIGen Llama 1B, short answer | -0.5% | +5.8% | Pass |
| APIGen Llama 1B, generation before embedding | -1.9% | -13.2% | Pass |
| APIGen Llama 1B, generation after embedding | -1.5% | -4.3% | Pass |
| APIGen Llama 1B, first embedding | -5.8% | +1.7% | Pass |
| APIGen Llama 1B, repeated embedding | -4.8% | -11.9% | Pass |
| Gemma 4 E4B, OCR | +28.1% | +27.2% | Fail |

Every candidate workload above passed all 25 semantic checks. Both embedding
fixtures had cosine agreement of `1.000000000`. The structured extraction
fixture passed 25/25 on the candidate and 0/25 on native, so it has no valid
successful-output latency comparison.

Median readiness increased by 148 ms for the Llama text comparison, 133 ms
for mixed operations, and 202 ms for Gemma OCR. All meet the startup allowance.
Peak sampled PSS increased by about 40 MiB for text, decreased by about
124 MiB for mixed operations, and increased by about 48 MiB for OCR. These
meet the memory-growth allowance. Cgroup peaks and RSS high-water values are
also retained in the reports; cgroup accounting and PSS are not interchangeable.

Raw evidence and generated tables:

- `target/agent-tmp/no-fit-text-full.{json,md}`
- `target/agent-tmp/no-fit-mixed-full.{json,md}`
- `target/agent-tmp/no-fit-ocr-full.{json,md}`
- `target/agent-tmp/startup-detail.log`, the pre-fix startup diagnostic

The OCR median is 10.481 seconds versus native's 8.179 seconds. This exceeds
the approved 5% median and 10% p95 regression limits and blocks replacement.
Its cause is not yet established. Earlier short, version-matched diagnostic
runs are not substitutes for this full comparison. Accelerator validation,
remaining conformance coverage, and the production cutover are still pending.

A diagnostic image with flash attention disabled also passed OCR correctness
but remained 26.1% slower in a two-warm-request check. This check ran with
accelerator builds active and is not an acceptance measurement. It does not
support disabling flash attention as the fix. Evidence is in
`target/agent-tmp/no-fa-ocr-control.{json,md}`; the diagnostic image is separate
from the candidate.
