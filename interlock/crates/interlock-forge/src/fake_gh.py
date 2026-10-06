#!/usr/bin/env python3
"""A stand-in for the GitHub CLI, for interlock's tests. No network.

State is a JSON file named by FAKE_GH_STATE, or gh-state.json beside this
script. Pull requests live there;
branches live in the bare git repository named by its "remote" key, which the
code under test pushes to with plain git, as it would to GitHub.

Faults are set per subcommand ("create", "list", "view", "merge") as
{"mode": ..., "times": n}; a negative n never runs out. Modes:
  unreachable  any subcommand: no effect, and an answer like a network error
  refuse       merge: refused by branch policy
  move_head    merge: someone pushes to the head branch while the merge is in flight
  crash        merge: merge, then kill the caller before it hears back
  die          merge: merge, then fail without saying it merged
  vanish       merge: merge, then stop answering anything until faults are cleared
  queue        merge: accept the request, and merge only a few calls later
  queue_then_drop  merge: like queue, but the call then fails like a dropped connection
  hang         any subcommand: no answer for "hang_seconds" (default 30), then fail
  orphan       any subcommand: answer normally, but leave a child holding the output open
"checks" is "pass", "pending", "fail" or "none" (no checks reported).
Other knobs: "lag" {"head": sha, "reads": n} reports that head for the next n
reads of an open pull request; "base_oid" reports that as baseRefOid, as a
lagging API would; a pull request with "lie_merged" is reported merged at its
head without anything merged. Every call is appended to "calls". When "db"
names interlock's store, each call also records in "seen" the operation rows
that were committed when the call arrived.
"""

import datetime
import json
import os
import signal
import sqlite3
import subprocess
import sys
import time

STATE = os.environ.get("FAKE_GH_STATE") or os.path.join(os.path.dirname(os.path.abspath(__file__)), "gh-state.json")
URL = "https://github.com/fake/repo/pull/{}"
IDENTITY = {
    "GIT_AUTHOR_NAME": "fake-gh",
    "GIT_AUTHOR_EMAIL": "fake-gh@localhost",
    "GIT_COMMITTER_NAME": "fake-gh",
    "GIT_COMMITTER_EMAIL": "fake-gh@localhost",
}


class Exit(Exception):
    def __init__(self, code, message=""):
        super().__init__(message)
        self.code = code
        self.message = message


def fail(message, code=1):
    raise Exit(code, message)


def git(st, *args, check=True):
    env = dict(os.environ, **IDENTITY)
    p = subprocess.run(
        ["git", "--git-dir", st["remote"], "-c", "commit.gpgsign=false", *args],
        capture_output=True,
        text=True,
        env=env,
    )
    if check and p.returncode != 0:
        fail(f"fake gh: git {' '.join(args)}: {p.stderr.strip()}", 3)
    return p


def tip(st, branch):
    p = git(st, "rev-parse", "--verify", "--quiet", f"refs/heads/{branch}^{{commit}}", check=False)
    return p.stdout.strip() or None


def fault(st, name):
    f = st.get("faults", {}).get(name)
    if not f or f.get("times", 1) == 0:
        return None
    if f.get("times", 1) > 0:
        f["times"] = f.get("times", 1) - 1
    return f["mode"]


def unreachable():
    fail("error connecting to api.github.com\ncheck your internet connection or https://githubstatus.com")


def parse(args, values, flags=None):
    """gh-style flags: values maps a flag to a key that takes a value; flags to a boolean key."""
    flags = flags or {}
    pos, out, i = [], {}, 0
    while i < len(args):
        a = args[i]
        if a.startswith("--") and "=" in a:
            k, v = a.split("=", 1)
            if k not in values:
                fail(f"unknown flag: {k}", 2)
            out[values[k]] = v
        elif a in values:
            if i + 1 >= len(args):
                fail(f"flag needs an argument: {a}", 2)
            out[values[a]] = args[i + 1]
            i += 1
        elif a in flags:
            out[flags[a]] = True
        elif a.startswith("-"):
            fail(f"unknown flag: {a}", 2)
        else:
            pos.append(a)
        i += 1
    return pos, out


def merge_tree(st, base, head):
    p = git(st, "merge-tree", "--write-tree", "--no-messages", base, head, check=False)
    return p.stdout.split("\n")[0].strip() if p.returncode == 0 else None


def do_merge(st, pr, method):
    base, head = tip(st, pr["base"]), tip(st, pr["head"])
    tree = merge_tree(st, base, head)
    if tree is None:
        return f"X Pull request #{pr['number']} is not mergeable: the merge commit cannot be cleanly created."
    if method == "merge":
        message = f"Merge pull request #{pr['number']} from {pr['head']}"
        commit = git(st, "commit-tree", tree, "-p", base, "-p", head, "-m", message).stdout.strip()
    else:
        message = f"{pr['title']} (#{pr['number']})"
        commit = git(st, "commit-tree", tree, "-p", base, "-m", message).stdout.strip()
    git(st, "update-ref", f"refs/heads/{pr['base']}", commit, base)
    pr.update(state="MERGED", merged_head=head, merge_commit=commit, auto_merge=None, merged_at=now())
    return None


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat().replace("+00:00", "Z")


def checks_pass(st):
    # With no checks at all, nothing is required: the branch is clean.
    return st.get("checks", "pass") in ("pass", "none")


def settle_auto_merges(st):
    """Time passes: an armed auto-merge whose checks now pass merges, as GitHub would,
    and a queued merge completes once its countdown of calls runs out."""
    for pr in st["prs"]:
        queued = pr.get("queued")
        if pr["state"] == "OPEN" and queued:
            queued["after"] -= 1
            if queued["after"] <= 0:
                pr["queued"] = None
                do_merge(st, pr, queued["method"])
            continue
        armed = pr.get("auto_merge")
        if pr["state"] != "OPEN" or not armed or not checks_pass(st):
            continue
        if armed.get("head") and not (tip(st, pr["head"]) or "").startswith(armed["head"]):
            pr["auto_merge"] = None  # GitHub turns auto-merge off when the head changes
        else:
            do_merge(st, pr, armed["method"])


def rollup(st):
    checks = st.get("checks", "pass")
    if checks == "pass":
        return [{"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"}]
    if checks == "pending":
        return [{"__typename": "CheckRun", "name": "ci", "status": "IN_PROGRESS", "conclusion": ""}]
    if checks == "none":
        return []
    return [{"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "FAILURE"}]


def render(st, pr, fields):
    is_open = pr["state"] == "OPEN"
    head = tip(st, pr["head"]) if is_open else pr.get("merged_head") or pr.get("closed_head") or ""
    base = tip(st, pr["base"]) or ""
    state, merge_commit, merged_at = pr["state"], pr.get("merge_commit"), pr.get("merged_at")
    if is_open and pr.get("lie_merged"):
        # A forge (or a forged gh) that claims a merge nothing performed.
        state, merge_commit, merged_at = "MERGED", head, now()
    shown_head = head
    lag = st.get("lag")
    if is_open and lag and lag.get("reads", 0) > 0:
        shown_head = lag["head"]
        lag["reads"] -= 1
    mergeable, merge_state = "UNKNOWN", "UNKNOWN"
    if is_open and head and base:
        mergeable = "MERGEABLE" if merge_tree(st, base, head) else "CONFLICTING"
        if mergeable == "CONFLICTING":
            merge_state = "DIRTY"
        elif not checks_pass(st):
            merge_state = "BLOCKED"
        else:
            merge_state = "CLEAN"
    full = {
        "number": pr["number"],
        "url": URL.format(pr["number"]),
        "state": state,
        "title": pr["title"],
        "body": pr["body"],
        "isDraft": False,
        "headRefName": pr["head"],
        "headRefOid": shown_head,
        "baseRefName": pr["base"],
        "baseRefOid": st.get("base_oid") or base,
        "mergeable": mergeable,
        "mergeStateStatus": merge_state,
        "statusCheckRollup": rollup(st) if head else [],
        "mergeCommit": {"oid": merge_commit} if merge_commit else None,
        "mergedAt": merged_at,
        "autoMergeRequest": {"mergeMethod": pr["auto_merge"]["method"].upper()} if pr.get("auto_merge") else None,
    }
    out = {}
    for f in fields:
        if f not in full:
            fail(f'Unknown JSON field: "{f}"')
        out[f] = full[f]
    return out


def find(st, selector):
    sel = selector.rsplit("/pull/", 1)[-1]
    for pr in st["prs"]:
        if str(pr["number"]) == sel or pr["head"] == selector:
            return pr
    fail(f"GraphQL: Could not resolve to a PullRequest with the number of {sel}. (repository.pullRequest)")


def pr_create(st, args):
    if fault(st, "create") == "unreachable":
        unreachable()
    _, o = parse(args, {
        "--head": "head", "-H": "head", "--base": "base", "-B": "base",
        "--title": "title", "-t": "title", "--body": "body", "-b": "body",
    })
    head, base = o.get("head"), o.get("base")
    if not head or not base:
        fail("fake gh: pass --head and --base", 2)
    head_tip, base_tip = tip(st, head), tip(st, base)
    if not head_tip or not base_tip:
        fail("pull request create failed: GraphQL: Head sha can't be blank, Base sha can't be blank (createPullRequest)")
    if git(st, "merge-base", "--is-ancestor", head_tip, base_tip, check=False).returncode == 0:
        fail(f"pull request create failed: GraphQL: No commits between {base} and {head} (createPullRequest)")
    for pr in st["prs"]:
        if pr["head"] == head and pr["base"] == base and pr["state"] == "OPEN":
            fail(f'a pull request for branch "{head}" into branch "{base}" already exists:\n{URL.format(pr["number"])}')
    n = len(st["prs"]) + 1
    st["prs"].append({
        "number": n, "head": head, "base": base, "title": o.get("title", ""), "body": o.get("body", ""),
        "state": "OPEN",
    })
    print(URL.format(n))


def pr_list(st, args):
    if fault(st, "list") == "unreachable":
        unreachable()
    _, o = parse(args, {
        "--head": "head", "-H": "head", "--base": "base", "-B": "base", "--state": "state", "-s": "state",
        "--json": "json", "--limit": "limit", "-L": "limit",
    })
    if "json" not in o:
        fail("fake gh: only --json output is supported", 2)
    state = o.get("state", "open").upper()
    prs = [
        p for p in st["prs"]
        if (not o.get("head") or p["head"] == o["head"])
        and (not o.get("base") or p["base"] == o["base"])
        and (state == "ALL" or p["state"] == state)
    ]
    prs.sort(key=lambda p: -p["number"])
    prs = prs[: int(o.get("limit", 30))]
    print(json.dumps([render(st, p, o["json"].split(",")) for p in prs]))


def pr_view(st, args):
    if fault(st, "view") == "unreachable":
        unreachable()
    pos, o = parse(args, {"--json": "json"})
    if not pos or "json" not in o:
        fail("fake gh: pr view needs a number and --json", 2)
    print(json.dumps(render(st, find(st, pos[0]), o["json"].split(","))))


def pr_merge(st, args):
    pos, o = parse(
        args,
        {"--match-head-commit": "match", "--subject": "subject", "-t": "subject", "--body": "body", "-b": "body"},
        {
            "--merge": "merge", "-m": "merge", "--squash": "squash", "-s": "squash", "--rebase": "rebase",
            "-r": "rebase", "--auto": "auto", "--delete-branch": "delete", "-d": "delete", "--admin": "admin",
            "--disable-auto": "disable_auto",
        },
    )
    mode = fault(st, "merge")
    if mode == "unreachable":
        unreachable()
    if not pos:
        fail("fake gh: pr merge needs a number", 2)
    pr = find(st, pos[0])
    n = pr["number"]
    if o.get("disable_auto"):
        if pr["state"] != "OPEN":
            fail(f"X Pull request #{n} is not open")
        pr["auto_merge"] = None
        sys.stderr.write(f"✓ Auto-merge disabled for pull request #{n}\n")
        return
    if pr["state"] == "MERGED":
        sys.stderr.write(f"! Pull request #{n} was already merged\n")
        return
    if pr["state"] == "CLOSED":
        fail(f"X Pull request #{n} is closed")
    method = next((m for m in ("merge", "squash", "rebase") if o.get(m)), None)
    if not method:
        fail("--merge, --rebase, or --squash required when not running interactively")
    if mode == "move_head":
        # Someone pushes to the head branch while the merge request is in flight.
        old = tip(st, pr["head"])
        tree = git(st, "rev-parse", f"{old}^{{tree}}").stdout.strip()
        moved = git(st, "commit-tree", tree, "-p", old, "-m", "a push nobody verified").stdout.strip()
        git(st, "update-ref", f"refs/heads/{pr['head']}", moved, old)
    if mode == "refuse":
        fail(f"X Pull request #{n} is not mergeable: the base branch policy prohibits the merge.")
    head = tip(st, pr["head"])
    if o.get("match") and not head.startswith(o["match"]):
        fail("GraphQL: Head branch was modified. Review and try the merge again. (mergePullRequest)")
    if o.get("auto") and not checks_pass(st):
        pr["auto_merge"] = {"method": method, "head": o.get("match")}
        sys.stderr.write(f"! Pull request #{n} will be automatically merged via {method} when all requirements are met\n")
        return
    if not checks_pass(st):
        fail(f"X Pull request #{n} is not mergeable: the base branch policy prohibits the merge.")
    if mode in ("queue", "queue_then_drop"):
        # Accepted, not merged yet: it merges a few calls later, as a merge queue would.
        pr["queued"] = {"method": method, "after": 3}
        if mode == "queue_then_drop":
            fail("Post https://api.github.com/graphql: read tcp: connection reset by peer")
        sys.stderr.write(f"✓ Pull request #{n} will be added to the merge queue for {pr['base']} when ready\n")
        return
    err = do_merge(st, pr, method)
    if err:
        fail(err)
    if mode == "crash":
        save(st)
        os.kill(os.getppid(), signal.SIGKILL)
        os._exit(137)
    if mode == "vanish":
        # The forge stops answering right after the merge, until the faults are cleared.
        st["faults"].update({c: {"mode": "unreachable", "times": -1} for c in ("view", "list", "create")})
        unreachable()
    if mode == "die":
        fail("fake gh: the connection was reset before the answer arrived")
    sys.stderr.write(f"✓ Merged pull request #{n} ({pr['title']})\n")


def save(st):
    tmp = STATE + ".tmp"
    with open(tmp, "w") as f:
        json.dump(st, f, indent=2)
    os.replace(tmp, STATE)


def strip_repo(args):
    out, i = [], 0
    while i < len(args):
        if args[i] in ("--repo", "-R"):
            i += 2
            continue
        if args[i].startswith("--repo="):
            i += 1
            continue
        out.append(args[i])
        i += 1
    return out


def main():
    args = sys.argv[1:]
    if args[:1] in (["--version"], ["version"]):
        print("gh version 0.0.0-fake (interlock tests)")
        return 0
    if not os.path.exists(STATE):
        sys.stderr.write(f"fake gh: no state file at {STATE}\n")
        return 2
    with open(STATE) as f:
        st = json.load(f)
    st.setdefault("prs", [])
    st.setdefault("calls", []).append(args)
    if st.get("db"):
        con = sqlite3.connect(f"file:{st['db']}?mode=ro", uri=True)
        rows = con.execute("SELECT id, kind, state FROM operations ORDER BY rowid").fetchall()
        con.close()
        st.setdefault("seen", []).append({"call": args, "operations": [list(r) for r in rows]})
    commands = {"create": pr_create, "list": pr_list, "view": pr_view, "merge": pr_merge}
    code = 0
    try:
        rest = strip_repo(args)
        if len(rest) < 2 or rest[0] != "pr" or rest[1] not in commands:
            fail(f"fake gh: unsupported command: {' '.join(args)}", 2)
        pending = st.get("faults", {}).get(rest[1], {}).get("mode")
        if pending == "hang" and fault(st, rest[1]):
            save(st)
            time.sleep(st.get("hang_seconds", 30))
            fail("fake gh: gave up after hanging")
        if pending == "orphan" and fault(st, rest[1]):
            # A child that outlives this process and keeps its output open.
            subprocess.Popen(["sleep", str(st.get("hang_seconds", 30))])
        settle_auto_merges(st)
        commands[rest[1]](st, rest[2:])
    except Exit as e:
        if e.message:
            sys.stderr.write(e.message + "\n")
        code = e.code
    save(st)
    return code


if __name__ == "__main__":
    sys.exit(main())
