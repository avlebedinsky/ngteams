#!/usr/bin/env python3
"""Make synthetic codec/container samples for the private Windows runtime smoke.

Requires FFmpeg with its usual software encoders. Output is disposable, not a
capture: FFmpeg's testsrc2 plus a 1 kHz sine. No device, account or network input.
"""

import argparse
import json
from pathlib import Path
import subprocess

# Cover both common Teams attachments and the other local containers/codecs in
# the Linux runtime. Encoders run only here; the Windows runtime must decode them.
FORMATS = {
    "h264-aac.mp4": ["-c:v", "libx264", "-c:a", "aac"],
    "hevc-aac.mov": ["-c:v", "libx265", "-x265-params", "log-level=error:pools=1", "-c:a", "aac"],
    "vp8-vorbis.webm": ["-c:v", "libvpx", "-c:a", "libvorbis"],
    "vp9-opus.webm": ["-c:v", "libvpx-vp9", "-c:a", "libopus"],
    "av1-opus.mkv": ["-c:v", "libaom-av1", "-cpu-used", "8", "-c:a", "libopus"],
    "theora-vorbis.ogv": ["-c:v", "libtheora", "-c:a", "libvorbis"],
    "mpeg4-mp3.avi": ["-c:v", "mpeg4", "-c:a", "libmp3lame"],
    "mpeg2-mp2.mpg": ["-c:v", "mpeg2video", "-c:a", "mp2"],
    "h264-aac.ts": ["-c:v", "libx264", "-c:a", "aac"],
    "flv-mp3.flv": ["-c:v", "flv", "-c:a", "libmp3lame"],
    "wmv-wma.asf": ["-c:v", "wmv2", "-c:a", "wmav2"],
    "pcm.wav": ["-vn", "-c:a", "pcm_s16le"],
    "pcm.aiff": ["-vn", "-c:a", "pcm_s16be"],
    "audio.flac": ["-vn", "-c:a", "flac"],
    "audio.mp3": ["-vn", "-c:a", "libmp3lame"],
    "audio.opus": ["-vn", "-c:a", "libopus"],
    "audio.wv": ["-vn", "-c:a", "wavpack"],
    "audio.spx": ["-vn", "-ar", "16000", "-c:a", "libspeex"],
    "audio.ac3": ["-vn", "-c:a", "ac3"],
    "audio.dts": ["-vn", "-c:a", "dca", "-strict", "-2"],
    "audio.aac": ["-vn", "-c:a", "aac", "-f", "adts"],
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    for name, codec in FORMATS.items():
        subprocess.run(["ffmpeg", "-nostdin", "-hide_banner", "-loglevel", "error", "-y",
                        "-f", "lavfi", "-i", "testsrc2=size=160x120:rate=25",
                        "-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=48000",
                        "-t", "0.4", "-threads", "1", *codec, str(args.output / name)], check=True)
    (args.output / "SYNTHETIC.json").write_text(json.dumps({"source": "FFmpeg testsrc2 and sine; no captured data",
                                                           "files": list(FORMATS)}, indent=2) + "\n")
    print(f"generated {len(FORMATS)} synthetic media samples in {args.output}")


if __name__ == "__main__":
    main()
