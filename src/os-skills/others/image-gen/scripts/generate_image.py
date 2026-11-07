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

def _response_error(context, detail):
    print(f"ERROR: {context}: {detail}", file=sys.stderr)
    raise SystemExit(1)


def _response_json(body, context):
    try:
        value = json.loads(body)
    except (json.JSONDecodeError, UnicodeError):
        _response_error(context, "response is not valid UTF-8 JSON")
    if not isinstance(value, dict):
        _response_error(context, "response must be a JSON object")
    return value


def _response_output(response, context):
    output = response.get("output", {})
    if not isinstance(output, dict):
        _response_error(context, "output must be an object")
    return output


def _image_results(value, base64_field, context):
    if not isinstance(value, list):
        _response_error(context, "image results must be a list")
    if not value:
        return None
    image = value[0]
    if not isinstance(image, dict):
        _response_error(context, "image result must be an object")
    for field in ("url", base64_field):
        content = image.get(field)
        if content is not None and not isinstance(content, str):
            _response_error(context, f"{field} must be a string")
    url = image.get("url")
    encoded = image.get(base64_field)
    return url or ("b64:" + encoded if encoded else None)


def _wanx(prompt, model, size, key):
    url = "https://dashscope.aliyuncs.com/api/v1/services/aigc/text2image/image-synthesis"
    h = {"Authorization":f"Bearer {key}","Content-Type":"application/json","X-DashScope-Async":"enable"}
    body = {"model":model,"input":{"prompt":prompt},"parameters":{"size":size,"n":1}}
    req = urllib.request.Request(url,json.dumps(body).encode(),h,method="POST")
    try:
        with urllib.request.urlopen(req,timeout=60) as r: res = _response_json(r.read(), "image request")
    except urllib.error.HTTPError as e:
        print(f"ERROR: HTTP {e.code} {e.read().decode() if e.readable() else ''}",file=sys.stderr); sys.exit(1)
    tid = _response_output(res, "image request").get("task_id")
    if not isinstance(tid, str) or not tid: print(f"ERROR: {json.dumps(res)}",file=sys.stderr); sys.exit(1)
    print(f"Task: {tid}",file=sys.stderr)
    ph = {"Authorization":f"Bearer {key}"}
    for i in range(120):
        time.sleep(2)
        req = urllib.request.Request(f"https://dashscope.aliyuncs.com/api/v1/tasks/{tid}",headers=ph)
        with urllib.request.urlopen(req,timeout=30) as r: st = _response_json(r.read(), "task poll")
        output = _response_output(st, "task poll")
        s = output.get("task_status", "")
        if not isinstance(s, str):
            _response_error("task poll", "task_status must be a string")
        if s == "SUCCEEDED":
            source = _image_results(output.get("results", []), "b64_image", "task result")
            if source: return source
        elif s == "FAILED":
            print(f"ERROR: {st['output'].get('message','')}",file=sys.stderr); sys.exit(1)
    print("ERROR: Timeout",file=sys.stderr); sys.exit(1)

def _compat(prompt, model, size, key, base):
    h = {"Authorization":f"Bearer {key}","Content-Type":"application/json"}
    body = {"model":model,"prompt":prompt,"size":size,"n":1,"response_format":"url"}
    req = urllib.request.Request(f"{base}/images/generations",json.dumps(body).encode(),h,method="POST")
    try:
        with urllib.request.urlopen(req,timeout=120) as r: res = _response_json(r.read(), "image request")
    except urllib.error.HTTPError:
        return _wanx(prompt, model, size, key)
    source = _image_results(res.get("data", []), "b64_json", "image response")
    if source: return source
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
