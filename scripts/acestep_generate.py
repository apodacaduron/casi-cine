#!/usr/bin/env python3
"""One-shot, offline ACE-Step adapter used by Casi Cine."""
from __future__ import annotations
import argparse, json, math, os, subprocess, sys, time
from pathlib import Path

def main() -> int:
    p=argparse.ArgumentParser();p.add_argument("--sidecar-root",required=True);p.add_argument("--output",required=True);p.add_argument("--duration",type=int,required=True);p.add_argument("--prompt",required=True);p.add_argument("--seed",type=int,required=True);a=p.parse_args()
    root=Path(a.sidecar_root).resolve();output=Path(a.output).resolve();cache=root/".cache";cache.mkdir(parents=True,exist_ok=True)
    os.environ.update({"HF_HOME":str(cache/"huggingface"),"HUGGINGFACE_HUB_CACHE":str(cache/"huggingface"/"hub"),"HF_MODULES_CACHE":str(cache/"huggingface"/"modules"),"MPLCONFIGDIR":str(cache/"matplotlib"),"XDG_CACHE_HOME":str(cache),"ACESTEP_LM_BACKEND":"mlx","HF_HUB_OFFLINE":"1","TRANSFORMERS_OFFLINE":"1"})
    sys.path.insert(0,str(root));from acestep.handler import AceStepHandler;from acestep.inference import GenerationConfig,GenerationParams,generate_music
    handler=AceStepHandler();started=time.monotonic();message,ok=handler.initialize_service(project_root=str(root),config_path="acestep-v15-turbo",device="mps",offload_to_cpu=False)
    if not ok:print(json.dumps({"success":False,"error":message}));return 2
    # Long latent sequences exhaust 16 GB unified-memory Macs. Generate one
    # short coherent theme, release the model process, then extend with gentle
    # crossfades in FFmpeg. This keeps peak memory bounded.
    output.parent.mkdir(parents=True,exist_ok=True);duration=max(10,min(a.duration,30))
    params=GenerationParams(task_type="text2music",thinking=False,caption=a.prompt,lyrics="[Instrumental]",instrumental=True,bpm=88,keyscale="D major",timesignature="4/4",vocal_language="en",duration=duration,inference_steps=8,guidance_scale=1.0,seed=a.seed)
    config=GenerationConfig(batch_size=1,audio_format="wav",use_random_seed=False,seeds=[a.seed]);result=generate_music(handler,None,params=params,config=config,save_dir=str(output.parent))
    if not result.success or not result.audios:print(json.dumps({"success":False,"error":result.status_message}));return 3
    generated=Path(result.audios[0]["path"])
    if a.duration <= duration:
        generated.replace(output)
    else:
        overlap=3;copies=max(2,math.ceil((a.duration-overlap)/(duration-overlap)));cmd=["ffmpeg","-hide_banner","-loglevel","error","-y"]
        for _ in range(copies):cmd.extend(["-i",str(generated)])
        chains=[];previous="0:a"
        for index in range(1,copies):
            label=f"mix{index}";chains.append(f"[{previous}][{index}:a]acrossfade=d={overlap}:c1=tri:c2=tri[{label}]");previous=label
        cmd.extend(["-filter_complex",";".join(chains),"-map",f"[{previous}]","-t",str(a.duration),"-ar","48000","-ac","2",str(output)])
        subprocess.run(cmd,check=True);generated.unlink(missing_ok=True)
    print(json.dumps({"success":True,"path":str(output),"seed":a.seed,"seconds":round(time.monotonic()-started,3),"engine":"ace-step-1.5-mlx"}));return 0
if __name__=="__main__":raise SystemExit(main())
