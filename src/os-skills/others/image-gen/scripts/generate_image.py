#!/usr/bin/env python3
"""DashScope image generation (wanx models)."""
import argparse, base64, json, os, sys, time, urllib.request, urllib.error

def _key():
    # 1. 环境变量
    for v in ["DASHSCOPE_API_KEY","QWEN_API_KEY","OPENAI_API_KEY"]:
        k = os.environ.get(v)
        if k: return k
    # 2. 从 ~/.openclaw/openclaw.json 读取
    for cfg in [os.path.expanduser("~/.openclaw/openclaw.json")]:
        try:
            with open(cfg) as f: c = json.loads(f.read())
            # 格式1: {"providers":{"dashscope":{"apiKey":"..."}}}
            for p in (c.get("providers") or {}).values():
                if isinstance(p,dict) and p.get("apiKey"): return p["apiKey"]
            # 格式2: {"models":{"providers":{"xxx":{"apiKey":"..."}}}}
            for p in (c.get("models",{}).get("providers") or {}).values():
                if isinstance(p,dict) and p.get("apiKey"): return p["apiKey"]
        except (FileNotFoundError, json.JSONDecodeError, KeyError):
            pass
    print("ERROR: No API key. Set DASHSCOPE_API_KEY or configure ~/.openclaw/openclaw.json",file=sys.stderr); sys.exit(1)

def _wanx(prompt, model, size, key):
    url = "https://dashscope.aliyuncs.com/api/v1/services/aigc/text2image/image-synthesis"
    h = {"Authorization":f"Bearer {key}","Content-Type":"application/json","X-DashScope-Async":"enable"}
    body = {"model":model,"input":{"prompt":prompt},"parameters":{"size":size,"n":1}}
    req = urllib.request.Request(url,json.dumps(body).encode(),h,method="POST")
    try:
        with urllib.request.urlopen(req,timeout=60) as r: res = json.loads(r.read())
    except urllib.error.HTTPError as e:
        print(f"ERROR: HTTP {e.code} {e.read().decode() if e.readable() else ''}",file=sys.stderr); sys.exit(1)
    tid = res.get("output",{}).get("task_id")
    if not tid: print(f"ERROR: {json.dumps(res)}",file=sys.stderr); sys.exit(1)
    print(f"Task: {tid}",file=sys.stderr)
    ph = {"Authorization":f"Bearer {key}"}
    for i in range(120):
        time.sleep(2)
        req = urllib.request.Request(f"https://dashscope.aliyuncs.com/api/v1/tasks/{tid}",headers=ph)
        with urllib.request.urlopen(req,timeout=30) as r: st = json.loads(r.read())
        s = st.get("output",{}).get("task_status","")
        if s == "SUCCEEDED":
            rs = st["output"].get("results",[])
            if rs and rs[0].get("url"): return rs[0]["url"]
            if rs and rs[0].get("b64_image"): return "b64:"+rs[0]["b64_image"]
        elif s == "FAILED":
            print(f"ERROR: {st['output'].get('message','')}",file=sys.stderr); sys.exit(1)
    print("ERROR: Timeout",file=sys.stderr); sys.exit(1)

def _compat(prompt, model, size, key, base):
    h = {"Authorization":f"Bearer {key}","Content-Type":"application/json"}
    body = {"model":model,"prompt":prompt,"size":size,"n":1,"response_format":"url"}
    req = urllib.request.Request(f"{base}/images/generations",json.dumps(body).encode(),h,method="POST")
    try:
        with urllib.request.urlopen(req,timeout=120) as r: res = json.loads(r.read())
    except urllib.error.HTTPError:
        return _wanx(prompt, model, size, key)
    d = res.get("data",[])
    if d: return d[0].get("url") or ("b64:"+d[0].get("b64_json",""))
    print("ERROR: No image",file=sys.stderr); sys.exit(1)

def _save(src, path):
    if src.startswith("b64:"):
        data = base64.b64decode(src[4:])
    else:
        with urllib.request.urlopen(urllib.request.Request(src,headers={"User-Agent":"Mozilla/5.0"}),timeout=60) as r: data = r.read()
    os.makedirs(os.path.dirname(os.path.abspath(path)),exist_ok=True)
    with open(path,"wb") as f: f.write(data)
    print(f"Saved {path} ({len(data)/1024:.1f}KB)")

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("-p","--prompt",required=True)
    ap.add_argument("-o","--output",required=True)
    ap.add_argument("-s","--size",default="1024*1024")
    ap.add_argument("-m","--model",default="wanx2.1-t2i-turbo")
    ap.add_argument("--api-base",default="https://dashscope.aliyuncs.com/compatible-mode/v1")
    a = ap.parse_args()
    key = _key()
    size = a.size.replace("x","*")
    wanx = ["wanx-v1","wanx2.1-t2i-turbo","wanx2.1-t2i-plus","wanx2.0-t2i-turbo"]
    src = _wanx(a.prompt,a.model,size,key) if a.model in wanx else _compat(a.prompt,a.model,size,key,a.api_base)
    _save(src, a.output)

if __name__ == "__main__":
    main()
