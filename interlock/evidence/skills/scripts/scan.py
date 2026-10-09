"""Scans the evidence folder for raw attempt tokens and other secrets.

A 64-hex string is a raw token when its SHA-256 equals an attempt's token_hash. The hashes come
from the stores named on the command line and from every attempts.json in the folder. Also flags
e-mail addresses and common credential shapes.

usage: scan.py EVIDENCE_DIR [STORE.db ...]
"""
import glob, hashlib, json, os, re, sqlite3, sys

ev = sys.argv[1]
hashes = set()
for db in sys.argv[2:]:
    for (rec,) in sqlite3.connect(db).execute("SELECT record FROM attempts"):
        hashes.add(json.loads(rec)["token_hash"])
for p in glob.glob(os.path.join(ev, "**", "attempts.json"), recursive=True):
    for a in json.load(open(p)):
        if a.get("token_hash"):
            hashes.add(a["token_hash"])
print("known token hashes:", len(hashes))
HEX = re.compile(r"(?<![0-9a-f])[0-9a-f]{64}(?![0-9a-f])")
CRED = re.compile(r"(ghp_|gho_|ghs_|github_pat_|sk-ant-|xox[bp]-|Bearer [A-Za-z0-9]|AKIA[0-9A-Z]{12})")
EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+\.[A-Za-z][A-Za-z0-9.-]*")
OK_EMAILS = {"sam@example.com", "ana@example.com", "noreply@anthropic.com"}
bad = 0
for root, _, files in os.walk(ev):
    for f in files:
        p = os.path.join(root, f)
        text = open(p, encoding="utf-8", errors="replace").read()
        found = [h for h in set(HEX.findall(text)) if hashlib.sha256(h.encode()).hexdigest() in hashes]
        if found:
            bad += 1
            print("RAW TOKEN", os.path.relpath(p, ev), len(found))
        for m in CRED.findall(text):
            bad += 1
            print("CREDENTIAL SHAPE", os.path.relpath(p, ev), m)
        for m in set(EMAIL.findall(text)) - OK_EMAILS:
            bad += 1
            print("EMAIL", os.path.relpath(p, ev), m)
print("problems:", bad)
