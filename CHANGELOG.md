# Changelog

All notable changes to gpulens are listed here. The format follows [Keep a Changelog](https://keepachangelog.com), and versions follow [Semantic Versioning](https://semver.org).

## [0.1.2] - 2026-10-18

### Changed
- The headline rewrite is now off by default (`narrate = true` turns it on). It woke the model whenever the situation changed, which on a busy desktop kept it loaded in video memory.

### Fixed
- A program the model could not identify was asked about again every five minutes forever. It is now remembered and retried after a week.

## [0.1.1] - 2026-10-18

### Changed
- A highlight resting on the top row now stays on whatever is first, so the list stays pinned to its top when the order changes or you change the sort. Move down once and the highlight follows that app again.

## [0.1.0] - 2026-10-17

First public release.

### Added
- Interactive terminal interface that groups GPU processes into apps, with a plain-language headline, GPU-time and video-memory bars, a per-app table, a stacked history chart, a detail pane and a hardware strip.
- NVIDIA support through NVML, loaded at run time so the binary starts on any machine and explains what is missing.
- Rules for about a hundred common programs, Steam game names, and container and systemd unit detection.
- Optional local language-model helper (llama.cpp, llama-swap, Ollama, LM Studio, vLLM): names unfamiliar programs once, rewrites the headline, and explains an app on request. Rewrites are rejected if they contain numbers that are not in the readings, and command lines are scrubbed before they reach a model.
- A calm layer: smoothed readings, steady row order, debounced state labels, bars that glide between samples, and exited apps that fade out instead of vanishing. `calm = false` and `animate = false` turn it off.
- Safe stop action with confirmation, refusing desktop and system apps, other users' processes and reused process ids.
- `gpulens snapshot` (text and JSON), `setup`, `doctor`, shell completions and a scripted `--demo` GPU.
- Colour, 256-colour, 16-colour, no-colour and ASCII rendering; layouts from 60×14 to wide, including a square window.

### Known limits
- Only NVIDIA on Linux, and only verified on one machine and one GPU.
- Multi-GPU, MIG, vGPU, containers and non-llama.cpp model servers are handled in code and tests but have not been run on real setups.
- If one GPU fails to read, updates stop for all GPUs.
- Unified-memory systems are not specially handled.

[0.1.2]: https://github.com/guitaripod/gpulens/releases/tag/0.1.2
[0.1.1]: https://github.com/guitaripod/gpulens/releases/tag/0.1.1
[0.1.0]: https://github.com/guitaripod/gpulens/releases/tag/0.1.0
