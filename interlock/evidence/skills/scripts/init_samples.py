"""Makes the two sample repositories for the live guided runs in DIR: writes them with
make_samples.py, then commits them, the investigation one in two commits so its history
holds the reason for the line in question.

usage: init_samples.py DIR
"""
import os
import shutil
import subprocess
import sys

dest = os.path.abspath(sys.argv[1])
here = os.path.dirname(os.path.abspath(__file__))
os.makedirs(dest, exist_ok=True)
shutil.copy(os.path.join(here, "make_samples.py"), dest)
subprocess.run([sys.executable, os.path.join(dest, "make_samples.py")], check=True)


def git(repo, *args, who=("Sam", "sam@example.com")):
    env = dict(os.environ, GIT_AUTHOR_NAME=who[0], GIT_AUTHOR_EMAIL=who[1], GIT_COMMITTER_NAME=who[0],
               GIT_COMMITTER_EMAIL=who[1])
    for k in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"):
        env.pop(k, None)
    subprocess.run(["git", "-C", repo, *args], check=True, env=env, stdout=subprocess.DEVNULL)


bug = os.path.join(dest, "bugfix-repo")
git(bug, "init", "-q", "-b", "main")
git(bug, "add", "-A")
git(bug, "commit", "-q", "-m", "Add parse_duration with tests")

inv = os.path.join(dest, "investigation-repo")
git(inv, "init", "-q", "-b", "main")
git(inv, "add", "-A")
git(inv, "commit", "-q", "-m", "Add TTLCache")
shutil.copy(os.path.join(dest, "cache_v2.py"), os.path.join(inv, "cache.py"))
git(inv, "add", "-A")
git(inv, "commit", "-q", "-m", "Keep cache entries through their last second (#12)", "-m",
    "On coarse clocks (time.monotonic on some platforms ticks in 15 ms steps) entries stored with ttl=1 "
    "expired on the first read after a one-tick advance. Serve an entry until it is strictly older than "
    "ttl, so ttl is the minimum lifetime callers can rely on.", who=("Ana", "ana@example.com"))
print("samples in", dest)
