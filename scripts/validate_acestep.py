#!/usr/bin/env python3
"""One-shot, offline ACE-Step validation for the optional Casi Cine sidecar."""

from __future__ import annotations

import json
import os
import sys
import time
from pathlib import Path


def main() -> int:
    sidecar = Path(os.environ.get("ACESTEP_ROOT", ".local-ai/ACE-Step-1.5")).resolve()
    output = Path(os.environ.get("ACESTEP_VALIDATION_OUTPUT", "artifacts/acestep-validation")).resolve()
    if not (sidecar / "checkpoints" / "acestep-v15-turbo").exists():
        print("ACE-Step checkpoints are not installed", file=sys.stderr)
        return 2
    sys.path.insert(0, str(sidecar))
    local_cache = sidecar / ".cache"
    local_cache.mkdir(parents=True, exist_ok=True)
    os.environ.setdefault("HF_HOME", str(local_cache / "huggingface"))
    os.environ.setdefault("HUGGINGFACE_HUB_CACHE", str(local_cache / "huggingface" / "hub"))
    os.environ.setdefault("HF_MODULES_CACHE", str(local_cache / "huggingface" / "modules"))
    os.environ.setdefault("MPLCONFIGDIR", str(local_cache / "matplotlib"))
    os.environ.setdefault("XDG_CACHE_HOME", str(local_cache))
    os.environ.setdefault("ACESTEP_LM_BACKEND", "mlx")

    from acestep.handler import AceStepHandler
    from acestep.inference import GenerationConfig, GenerationParams, generate_music

    output.mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    handler = AceStepHandler()
    message, initialized = handler.initialize_service(
        project_root=str(sidecar),
        config_path="acestep-v15-turbo",
        device="mps",
        offload_to_cpu=False,
    )
    init_seconds = time.monotonic() - started
    if not initialized:
        print(f"ACE-Step initialization failed: {message}", file=sys.stderr)
        return 3

    params = GenerationParams(
        task_type="text2music",
        thinking=False,
        caption=(
            "Original instrumental cinematic family travel soundtrack, nostalgic and peaceful, "
            "warm felt piano, gentle acoustic guitar, soft strings, subtle brushed percussion, "
            "one continuous musical theme, natural dynamics, no vocals, no choir, no spoken words"
        ),
        lyrics="[Instrumental]",
        instrumental=True,
        bpm=86,
        keyscale="D major",
        timesignature="4/4",
        vocal_language="en",
        duration=12,
        inference_steps=8,
        guidance_scale=1.0,
        seed=20261103,
    )
    config = GenerationConfig(
        batch_size=1,
        audio_format="wav",
        use_random_seed=False,
        seeds=[20261103],
    )
    generation_started = time.monotonic()
    result = generate_music(handler, None, params=params, config=config, save_dir=str(output))
    generation_seconds = time.monotonic() - generation_started
    report = {
        "initialized": initialized,
        "initialization_seconds": round(init_seconds, 3),
        "generation_seconds": round(generation_seconds, 3),
        "success": result.success,
        "status": result.status_message,
        "audios": result.audios,
    }
    report_path = output / "validation.json"
    report_path.write_text(json.dumps(report, indent=2, default=str), encoding="utf-8")
    print(json.dumps(report, indent=2, default=str))
    return 0 if result.success else 4


if __name__ == "__main__":
    raise SystemExit(main())
