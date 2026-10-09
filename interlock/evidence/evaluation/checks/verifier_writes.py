"""Verifier shell commands that look like writes, from a run directory's transcripts.
Written by the v4 reviewer. A broad pattern: read every match by hand.

    python3 verifier_writes.py RUN_DIR      # e.g. ../claude-code-v4
"""
import json, gzip, glob, os, re, sys
root = sys.argv[1]
pat = re.compile(r"(>>?\s*(?!/tmp|/dev/null|&)[^\s;&|]+)|(\bcp\s+\S+\s+(?!/tmp)\.?/?[\w.]+)|(\bsed\s+-i)|(\btee\s+(?!/tmp))|(\btouch\s)|(\bmv\s)|(\brm\s)|(cat\s*>\s*(?!/tmp)[\w./]+)")
for run in sorted(glob.glob(os.path.join(root, "runs", "*.interlock.*"))):
    rj = json.load(open(os.path.join(run, "run.json")))
    for s in rj["sessions"]:
        if s.get("role") != "verifier":
            continue
        t = os.path.join(run, s["transcript"] + ".gz")
        if not os.path.exists(t):
            print("missing", t); continue
        with gzip.open(t, "rt") as f:
            for l in f:
                try:
                    e = json.loads(l, strict=False)
                except Exception:
                    continue
                if e.get("type") != "assistant":
                    continue
                for c in e["message"].get("content", []):
                    if c.get("type") == "tool_use":
                        cmd = (c.get("input") or {}).get("command") or json.dumps(c.get("input"))
                        # strip heredoc bodies for matching
                        hits = [m.group(0) for m in pat.finditer(cmd)]
                        hits = [h for h in hits if "2>&1" not in h and not h.startswith(">&")]
                        if hits:
                            print(os.path.basename(run), c.get("name"), hits, "|", cmd[:300].replace("\n", "\\n"))
