# GPUI video rendering: 9 × 720p at 30 fps

Date: 2026-10-19/20. M0 Task 16 (spike 2). Code: `spikes/gpui-video` (`src/frames.rs`, `src/grid.rs`, `src/bench.rs`,
`tests/bench.rs`). Raw results: [`gpui-video-runs.txt`](gpui-video-runs.txt).

## Question
Can GPUI show a 3×3 grid of 1280×720 video streams at 30 fps with a p95 frame time under 33 ms, on the Intel Iris Xe
iGPU and on the RTX 4070 Laptop dGPU? With stock GPUI (approach (a)), or only with a GPUI patch (approach (b))?

## Answer
- **Stock GPUI misses the target on both GPUs.** Its only way to draw pixels is a `RenderImage`, which goes through
  the sprite atlas: every new frame is a new 1280×1024 atlas texture, a copy of the pixels into the upload queue and a
  `write_texture`, and the old texture is freed. GPUI's render thread is the bottleneck (69–80% of a core); tiles get
  19–29 fps, with p95 frame times of 36–62 ms.
- **GPUI with agterm-rs's external-surfaces patch meets it** on the dGPU in every valid run and on the iGPU when the
  machine wasn't overloaded: 29.4–29.9 fps per tile, p95 frame time 15–19 ms, render thread at 43–48% of a core (64% in the loaded run 4). Each
  tile keeps one texture and each new frame is one `write_texture` straight from the frame's buffer.
- **Decision: approach (b).** GPUI is patched (vendored crates.io sources plus agterm-rs's patches, see below).

## Headline numbers
Best valid runs, 20 s measured after a 3 s warm-up, release build, 30 fps per stream, CPU% where 100% is one core.
"frame time" is GPUI's own frame duration (draw + present on the CPU, `ZED_MEASUREMENTS=1`); "interval" is the time
between consecutive draws; "fps per tile" counts distinct frames that reached a draw.

| | iGPU (a) stock | iGPU (b) patched | dGPU (a) stock | dGPU (b) patched |
|---|---|---|---|---|
| run (load average 1 min at start) | 2 (10.9) | 3 (16.9) | 2 (12.5) | 3 (20.7) |
| fps per tile (target 30) | 19.3 | **29.4** | 28.9 | **29.8** |
| frames skipped / received | 1937 / 5404 | 112 / 5400 | 214 / 5414 | 36 / 5401 |
| draws per second | 19.4 | 55.0 | 49.4 | 59.2 |
| frame time p50 / p95 | 40.4 / 61.2 ms | **7.1 / 18.7 ms** | 14.2 / 35.7 ms | **8.4 / 15.5 ms** |
| draw interval p50 / p95 | 50.8 / 73.5 ms | 16.7 / 30.4 ms | 16.8 / 37.2 ms | 16.7 / 17.3 ms |
| latency source → draw p50 / p95 | 16.6 / 31.7 ms | 8.8 / 19.8 ms | 9.4 / 25.1 ms | 8.4 / 16.3 ms |
| CPU total | 298% | 238% | 240% | 244% |
| of it: GPUI render thread | 79% | 48% | 69% | 43% |
| of it: 9 source threads (NV12 → BGRA) | 219% | 190% | 170% | 200% |
| GPU memory | 235 MiB (i915 fdinfo) | 158 MiB (i915 fdinfo) | 186 MiB (nvidia-smi) | 138 MiB (nvidia-smi) |

Every run in [`gpui-video-runs.txt`](gpui-video-runs.txt) (runs 1–2 on the crates.io GPUI, runs 3–5 on the patched
build, where the image path is unchanged stock code):

- (a) never reached the target: iGPU 19.3–24.8 fps per tile, dGPU 25.6–28.9; p95 frame time 36–62 ms.
- (b) on the dGPU: 29.8 and 29.9 fps, p95 15.5 and 18.1 ms (runs 3 and 4).
- (b) on the iGPU: 29.4 fps, p95 18.7 ms in run 3; **27.8 fps, p95 36.1 ms in run 4**, started at load 20 with a
  5-minute load of 33.
- Run 5 is void: the load climbed from 12 to 49 during it and every path fell to 5–13 fps.

## How it was measured
- **Source.** One thread per stream (`video-src-N`), 9 streams staggered over one frame period. Every 1/30 s a thread
  makes its stream's next NV12 frame (a copy of a pre-made background, a moving bar and the burned-in counter, the way
  a decoder hands over a frame) and converts it to BGRA on the CPU (BT.601 limited range, scalar code): GPUI can't
  take NV12 on either path. The conversion is the "source threads" CPU above: 5–10 ms (p50) per frame per thread.
- **Grid.** The frames go to GPUI's foreground through a channel; the grid keeps the newest frame per tile and asks
  for a redraw. (a): a fresh `Arc<RenderImage>` per frame, the replaced one dropped from the atlas
  (`Window::drop_image`). (b): a `PixelFrame` of the tile's `PixelSurface`, painted with
  `Window::paint_external_surface` from a `canvas`.
- **Read-back.** Each frame carries a 24-bit counter and its complement as black/white cells
  (`frames::read_counter`). After the measured phase, two window screenshots taken through the nested niri a second
  apart are read back: all 9 tiles must decode and advance (they did in every run). So the frames really reach the
  screen, and the numbers aren't measuring a grid that stopped updating.
- **Stats.** p50/p95 by nearest rank (`bench::Stats`). CPU from `/proc/<pid>/task/*/schedstat` over 95% of the
  measured phase, split into the main (render) thread, the source threads and the rest. GPU memory is the largest of
  3 samples: the resident memory in the process's DRM fdinfo (i915) and `nvidia-smi -q -d PIDS` for the NVIDIA
  driver. Both see the GPUI client only, not the compositors.
- **GPU choice.** `ZED_DEVICE_ID=0xa7a0` (Iris Xe, Mesa Vulkan) or `0x2820` (RTX 4070, NVIDIA Vulkan). The runner
  checks from GPUI's log that the requested PCI device rendered (`sandbox::check_selected_gpu`).

### Frame pacing in the headless compositor
Every run went to a private headless weston 15 (GL renderer, 1920×1080, `--refresh-rate=60000`) with a niri 26.04
nested in it (output 1280×800, the window full-screen in it, so each tile shows at 426×266). Nothing ran on the
desktop session. weston repaints on a 60 Hz timer; niri draws when weston's frame callback arrives, and GPUI draws
when niri's does. So draws are capped at 60 per second, as on a 60 Hz display, and with 9 staggered 30 fps streams
nearly every callback has a new frame (≈55–59 draws/s on (b)). The p50 interval of 16.7 ms is that cadence.

What this means for the 33 ms p95 target:
- **The frame time (draw + present on the CPU) doesn't depend on the compositor.** It's the work GPUI does per
  frame. On a real display it would be the same, plus vblank waits that this setup lacks: present never blocks here.
- **It doesn't include the GPU's own time** (texture copies and sampling). On (b) that's 9 × 3.5 MiB copies and a
  scaled draw per frame, well inside the budget on both GPUs, but not measured separately.
- **The compositors share the iGPU.** weston and niri render on the Iris Xe in every run, so the iGPU runs carry the
  compositing load a real session also has. With the dGPU selected, GPUI's buffers cross from the NVIDIA GPU to
  niri on the Intel GPU, which is the real path on a hybrid laptop too.
- **No scan-out, no vblank jitter.** A real display may add or hide a few ms of jitter per frame; the 15–19 ms p95
  on (b) leaves room for that, the 36–62 ms p95 on (a) doesn't.

### Load caveat
Other agents were building in parallel the whole time, so the machine was never quiet: the 1-minute load average was
10.9–28.7 at the start of the valid runs (20 CPUs: P-cores 0–11 and E-cores 12–19; every benchmark process could run
on all of them, `Cpus_allowed_list` 0–19). The process can be descheduled while it draws, and the iGPU shares memory
bandwidth with the builds, so **the numbers are pessimistic, and the load varied between runs by a factor of 2–3**.
A run with nobody else on the machine was not possible: the quietest moment we caught (load ≈ 11–12) went into run 2,
and the last attempt (run 5) saw the load climb to 49 and is void. Comparisons between (a) and (b) inside one run
(same load) are sound; absolute numbers are not lab-grade. The iGPU on (b) is the case closest to the limit:
under load 20 with a 5-minute load of 33 (run 4) it fell to 27.8 fps, p95 36 ms.

## Approach (b): the patch and what keeping it costs
- **What:** agterm-rs's `build/patches/gpui-pre/0001-external-surfaces.patch` (368 lines: `ExternalSurface`,
  `PixelFrame`, `DmaBufFrame`, `Window::paint_external_surface`) and
  `build/patches/gpui-pre-wgpu/0001-dmabuf-external-surfaces.patch` (1829 lines: the renderer side), copied unchanged
  into `build/patches/`. `scripts/vendor-gpui.sh` downloads the crates.io archives of `gpui-pre` and `gpui-pre-wgpu`
  0.3.9, checks them against the crates.io checksums, applies the patches into `vendor/` (3.7 MB, 121 files, outside
  the workspace members), and `--check` rebuilds and diffs them. The workspace takes them through `[patch.crates-io]`.
- **Upkeep:** every `gpui-pre` bump means re-running the script and rebasing two patches on GPUI's renderer, which
  changes often. The same patches are maintained in agterm-rs, so the work is shared, and the patch is an upstream
  candidate (it's the Linux counterpart of macOS's `paint_surface`); if Zed takes it, the vendoring goes away.
- **Risk:** the wgpu half opens GPUI's Vulkan device through wgpu-hal with two more extensions
  (`VK_EXT_image_drm_format_modifier`, `VK_EXT_queue_family_foreign`) and adds `ash` 0.38; an adapter without them
  opens as before. That path changes how GPUI starts on every GPU, not only for video: the smoke tests (`tests/smoke.rs`)
  run on both GPUs on the patched build.
- **Not used yet:** the dmabuf half. Zero-copy from VA-API/NVDEC needs it plus an NV12 → RGB shader (the patch
  imports RGB dmabufs). Today the CPU does NV12 → BGRA, 170–240% of a core for 9 streams with scalar code. That's the
  next cost to cut: SIMD conversion (libyuv-style) as the cheap step, NV12 sampled in a shader or decoder dmabufs
  imported zero-copy as the real fix. It doesn't affect the render thread's numbers above.

## Not covered
- Real decoders and real streams (frames are synthetic, every pixel changes every frame; real video with damage
  rectangles would upload less on (b)).
- GPU-side timing (timestamp queries), scan-out on a real display, fractional scaling.
- Larger grids or 1080p tiles, and 60 fps streams.
