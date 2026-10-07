#!/usr/bin/env bash
# interlock's remaining tests, one command each. Run on Linux or macOS (bash
# 3.2 and BSD tools will do), or inside the image testkit/Dockerfile builds. Each command keeps what it saw under
# testkit-results/<command>-<time>/ and ends with one line, RESULT: PASS,
# FAIL or NOT RUN, which it also writes to RESULT.txt there.
# Exit status: 0 PASS, 1 FAIL, 3 NOT RUN, 2 a usage error.
#
# TESTING.md says what each test proves, what it costs, and what to send back.
#
# Commands:
#   doctor                 what each test needs, and what is missing here
#   suite                  build, then every test with both hosts required (no account, no cost)
#   github                 deliver a verified fix to a separate GitHub test repository
#   copilot [--model M]    the export-retry task through Copilot CLI and its real model
#   guided                 set up a guided Copilot session with the skills; prints what to type
#   guided-collect         collect what the last guided session recorded
#   eval [options]         the three-condition evaluation on Copilot's real model (eval --help)
#   netfs DIR              interlock must refuse a store on the network filesystem holding DIR
#   pack                   zip everything under testkit-results/ to send back

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESULTS="${INTERLOCK_TESTKIT_RESULTS:-$ROOT/testkit-results}"
BIN="$ROOT/target/release/interlock"
THIS_REPOSITORY="tonysuss/autonomous-claude-workflow-bundle"
# Credentials a run may see. Their values never stay in the results: see redact.
SECRET_VARS=(COPILOT_GITHUB_TOKEN GH_TOKEN GITHUB_TOKEN ANTHROPIC_API_KEY CLAUDE_CODE_OAUTH_TOKEN)
# The four stand-in tasks interlock did worst on before the v4 prompts.
HARDER_TASKS=(go-dotted-section go-duration-days py-thousands py-date-filter)

say() { printf '%s\n' "$*" >&2; }
usage() { sed -n '/^# Commands:/,/^$/p' "$0" | sed -e 's/^# \{0,1\}//' -e '/^$/d'; }
usage_error() {
  say "$*"
  usage >&2
  exit 2
}

first_line() { "$@" 2>/dev/null | head -n1; }
copilot_bin() { printf '%s' "${INTERLOCK_COPILOT_BIN:-$(command -v copilot || true)}"; }
claude_bin() { printf '%s' "${INTERLOCK_CLAUDE_BIN:-$(command -v claude || true)}"; }
# ver_ge HAVE WANT: whether version HAVE is at least WANT, part by numeric part.
ver_ge() {
  local IFS=. i h w
  local -a have want
  read -r -a have <<< "$1"
  read -r -a want <<< "$2"
  for ((i = 0; i < ${#want[@]}; i++)); do
    h="${have[i]:-0}" && h="${h%%[!0-9]*}" && h="${h:-0}"
    w="${want[i]}" && w="${w%%[!0-9]*}" && w="${w:-0}"
    ((10#$h > 10#$w)) && return 0
    ((10#$h < 10#$w)) && return 1
  done
  return 0
}
lower() { printf '%s' "$1" | tr '[:upper:]' '[:lower:]'; }
stamp() { date -u +%Y%m%dT%H%M%SZ; }
# Free space, in whole gigabytes, on the filesystem holding the build.
free_gb() { df -Pk "$ROOT" | awk 'NR == 2 { print int($4 / 1048576) }'; }
# json FILE EXPR: a value from a JSON file, by a Python expression on `d`.
json() { python3 -I -c 'import json, sys; d = json.load(open(sys.argv[1])); print(eval(sys.argv[2]))' "$1" "$2" 2>/dev/null; }

# The commit this copy was made from: stamped into testkit/COMMIT when the
# zip was made, or read from git in a checkout.
source_commit() {
  local stamped
  stamped="$(tr -d '[:space:]' < "$ROOT/testkit/COMMIT" 2>/dev/null)"
  if [[ "$stamped" =~ ^[0-9a-f]{40}$ ]]; then
    echo "$stamped"
  else
    git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo unknown
  fi
}

versions() {
  local c set=() v
  echo "interlock source commit: $(source_commit)"
  echo "os: $(uname -srm)"
  echo "rustc: $(first_line rustc --version)"
  echo "python3: $(first_line python3 --version)"
  echo "git: $(first_line git --version)"
  echo "go: $(first_line go version)"
  echo "node: $(first_line node --version)"
  c="$(copilot_bin)" && echo "copilot: ${c:+$(first_line "$c" --version)}"
  c="$(claude_bin)" && echo "claude: ${c:+$(first_line "$c" --version)}"
  echo "gh: $(first_line gh --version)"
  for v in "${SECRET_VARS[@]}" INTERLOCK_LIVE_GITHUB_REPO; do
    [[ -n "${!v:-}" ]] && set+=("$v")
  done
  echo "set in the environment (names only): ${set[*]:-none}"
}

OUT=""
begin() {
  local base n=2
  base="$RESULTS/$1-$(stamp)"
  OUT="$base"
  while [[ -e "$OUT" ]]; do OUT="$base-$n" && n=$((n + 1)); done
  mkdir -p "$OUT"
  versions > "$OUT/versions.txt"
  say "== $1: results in ${OUT#"$ROOT"/}"
}

# end PASS|FAIL|"NOT RUN" REASON: redact, record the verdict, and exit.
end() {
  local verdict="$1"
  shift
  redact "$OUT"
  printf 'RESULT: %s: %s\n' "$verdict" "$*" | tee "$OUT/RESULT.txt"
  case "$verdict" in
    PASS) exit 0 ;;
    FAIL) exit 1 ;;
    *) exit 3 ;;
  esac
}

# Replaces any credential value that reached DIR with [REDACTED]. A binary
# file holding one (a SQLite store) is removed instead, and REDACTED.txt says so.
redact() {
  python3 -I - "$1" "${SECRET_VARS[@]}" <<'PY'
import os, sys
root, names = sys.argv[1], sys.argv[2:]
secrets = [os.environ[n] for n in names if len(os.environ.get(n, "")) >= 8]
log = []
for d, _, files in os.walk(root):
    for f in files:
        p = os.path.join(d, f)
        try:
            data = open(p, "rb").read()
        except OSError:
            continue
        if not any(s.encode() in data for s in secrets):
            continue
        rel = os.path.relpath(p, root)
        try:
            text = data.decode()
        except UnicodeDecodeError:
            os.remove(p)
            log.append(f"removed {rel}: a binary file held a credential")
            continue
        for s in secrets:
            text = text.replace(s, "[REDACTED]")
        open(p, "w").write(text)
        log.append(f"redacted a credential in {rel}")
if log:
    with open(os.path.join(root, "REDACTED.txt"), "a") as out:
        out.write("\n".join(log) + "\n")
    print("\n".join(log), file=sys.stderr)
PY
}

build() {
  say "building interlock (release; the first build takes a few minutes)"
  (cd "$ROOT" && cargo build --release --locked -p interlock-cli) > "$OUT/build.txt" 2>&1 ||
    end FAIL "the build failed: see build.txt"
}

# il NAME ARGS...: runs interlock in the current directory, keeping stdout
# as NAME.json and stderr as NAME.err.
il() {
  local name="$1"
  shift
  "$BIN" "$@" > "$OUT/$name.json" 2> "$OUT/$name.err"
}

# A fresh git repository holding the export-retry example, at $WORK.
WORK=""
example_repo() {
  WORK="$(mktemp -d "${TMPDIR:-/tmp}/interlock-$1-XXXXXX")/export-retry"
  cp -r "$ROOT/examples/export-retry" "$WORK"
  find "$WORK" -name __pycache__ -prune -exec rm -rf {} +
  cd "$WORK" || end FAIL "cannot enter $WORK"
  if ! { git init -q && git add -A && git commit -qm "Exporter with retry"; }; then
    end FAIL "git could not commit the example"
  fi
}

# Keeps a task's records and its store, without worktrees or scratch checkouts.
collect_task() {
  local task="$1"
  il status status "$task"
  il transitions task log "$task"
  il attempts attempt list "$task"
  il notes note list --task "$task"
  il brief brief "$task"
  # A guided task keeps its verified output at a ref; a headless run records its tree.
  # Either way the repository's HEAD is the input snapshot: no branch moves.
  local tree
  tree="$(json "$OUT/status.json" 'd["task"].get("current_tree") or ""')"
  if git rev-parse -q --verify "refs/interlock/tasks/$task" > /dev/null; then
    git show --stat --patch "refs/interlock/tasks/$task" > "$OUT/verified-output.diff"
  elif [[ -n "$tree" ]]; then
    git diff --stat --patch HEAD "$tree" > "$OUT/verified-output.diff"
  else
    echo "no output tree: the task has no accepted result" > "$OUT/verified-output.diff"
  fi
  mkdir -p "$OUT/store"
  (cd .interlock && tar --exclude=./worktrees --exclude=./scratch -cf - .) | (cd "$OUT/store" && tar -xf -)
}

signals() { json "$OUT/transitions.json" '" ".join(r["signal"] for r in d)'; }
task_state() { json "$OUT/status.json" 'd["task"]["state"]'; }

# Copilot must reach its real model: these point it at a scripted one.
refuse_offline_copilot() {
  local v
  for v in COPILOT_OFFLINE $(compgen -e | grep '^COPILOT_PROVIDER_' || true); do
    [[ -n "${!v:-}" ]] && end "NOT RUN" "unset $v first: it points Copilot at a scripted model, not its real one"
  done
}

# Confirms with one small request that Copilot is signed in, and saves the
# host report to the store.
copilot_signed_in() {
  il host-inspect host inspect --host copilot --check-auth --save
  local auth
  auth="$(json "$OUT/host-inspect.json" 'd[0]["auth"]')"
  [[ "$auth" == ok ]] ||
    end "NOT RUN" "Copilot could not sign in (auth: ${auth:-unknown}). Run \`copilot login\`, or set COPILOT_GITHUB_TOKEN to a fine-grained token with the Copilot Requests permission; GH_TOKEN takes precedence over a login, so a token without that permission blocks it"
}

# ---------------------------------------------------------------- doctor

cmd_doctor() {
  local v c os=0 rust=0 py=0 gitok=0 gook=0 nodeok=0 cop=0 cla=0 ghok=0 ghauth=0 repo=0 token=""
  row() { printf '  %-5s %-30s %s\n' "$1" "$2" "$3"; }
  echo "Tools"
  case "$(uname -s)" in
    Linux | Darwin) os=1 && row ok "Linux or macOS" "$(uname -sr)" ;;
    *) row MISS "Linux or macOS" "interlock runs on Linux and macOS: use testkit/Dockerfile (TESTING.md, step 1)" ;;
  esac
  v="$(free_gb)"
  if [[ "$v" -ge 8 ]]; then row ok "8 GB of free disk" "$v GB"; else row MISS "8 GB of free disk" "$v GB: compiling the tests needs about 6 GB"; fi
  v="$(first_line rustc --version | awk '{print $2}')"
  if [[ -n "$v" ]] && ver_ge "$v" 1.89; then rust=1 && row ok "Rust 1.89 or later" "$v"; else row MISS "Rust 1.89 or later" "${v:-not found}: https://rustup.rs"; fi
  v="$(first_line python3 --version | awk '{print $2}')"
  if [[ -n "$v" ]] && ver_ge "$v" 3.11; then py=1 && row ok "Python 3.11 or later" "$v"; else row MISS "Python 3.11 or later" "${v:-not found}"; fi
  v="$(first_line git --version | awk '{print $3}')"
  if [[ -n "$v" ]] && ver_ge "$v" 2.31; then gitok=1 && row ok "git 2.31 or later" "$v"; else row MISS "git 2.31 or later" "${v:-not found}"; fi
  v="$(first_line go version | awk '{print $3}' | sed 's/^go//')"
  if [[ -n "$v" ]] && ver_ge "$v" 1.22; then gook=1 && row ok "Go 1.22 or later" "$v (the evaluation only)"; else row MISS "Go 1.22 or later" "${v:-not found} (the evaluation only)"; fi
  v="$(first_line node --version | sed 's/^v//')"
  if [[ -n "$v" ]] && ver_ge "$v" 22; then nodeok=1 && row ok "Node 22 or later" "$v"; else row MISS "Node 22 or later" "${v:-not found} (Copilot CLI needs it)"; fi
  c="$(copilot_bin)"
  v="${c:+$(first_line "$c" --version | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -n1)}"
  if [[ -n "$v" ]]; then
    cop=1 && row ok "Copilot CLI" "$v$([[ "$v" == 1.0.91 ]] || echo ' (verified with 1.0.91)')"
  else
    row MISS "Copilot CLI" "npm install -g @github/copilot@1.0.91"
  fi
  c="$(claude_bin)"
  v="${c:+$(first_line "$c" --version | awk '{print $1}')}"
  if [[ -n "$v" ]]; then cla=1 && row ok "Claude Code" "$v (the suite only; no account needed)"; else row MISS "Claude Code" "npm install -g @anthropic-ai/claude-code (the suite only)"; fi
  v="$(first_line gh --version | awk '{print $3}')"
  if [[ -n "$v" ]]; then ghok=1 && row ok "GitHub CLI" "$v"; else row MISS "GitHub CLI" "https://cli.github.com"; fi
  echo "Accounts"
  if [[ "$ghok" == 1 ]] && gh auth status > /dev/null 2>&1; then
    ghauth=1 && row ok "gh signed in" "$(gh api user --jq .login 2> /dev/null || echo yes)"
  else
    row MISS "gh signed in" "export GH_TOKEN (TESTING.md, step 3)"
  fi
  if [[ "${INTERLOCK_LIVE_GITHUB_REPO:-}" == */* && "$(lower "$INTERLOCK_LIVE_GITHUB_REPO")" != "$(lower "$THIS_REPOSITORY")" ]]; then
    repo=1 && row ok "test repository" "$INTERLOCK_LIVE_GITHUB_REPO"
  else
    row MISS "test repository" "export INTERLOCK_LIVE_GITHUB_REPO=<you>/interlock-sandbox (not $THIS_REPOSITORY)"
  fi
  for v in COPILOT_GITHUB_TOKEN GH_TOKEN GITHUB_TOKEN; do [[ -n "${!v:-}" ]] && token="$v" && break; done
  row "?" "Copilot signed in" "${token:+Copilot will use $token. }checked by \`run.sh copilot\` with one small request"
  echo "Tests"
  ready() {
    local name="$1" why="$2"
    shift 2
    local f
    for f in "$@"; do [[ "$f" == 1 ]] || { row no "$name" "$why"; return; }; done
    row ready "$name" ""
  }
  ready suite "needs Linux or macOS, Rust, Python, git, Copilot CLI and Claude Code" "$os" "$rust" "$py" "$gitok" "$cop" "$cla"
  ready github "needs Linux or macOS, Rust, git, gh signed in, and the test repository" "$os" "$rust" "$gitok" "$ghok" "$ghauth" "$repo"
  ready copilot "needs Linux or macOS, Rust, Python, git, Node and Copilot CLI (and a Copilot sign-in)" "$os" "$rust" "$py" "$gitok" "$nodeok" "$cop"
  ready guided "needs the same as copilot" "$os" "$rust" "$py" "$gitok" "$nodeok" "$cop"
  ready eval "needs the same as copilot, and Go" "$os" "$rust" "$py" "$gitok" "$nodeok" "$cop" "$gook"
  ready netfs "needs Linux or macOS, Rust, and a network mount" "$os" "$rust"
}

# ---------------------------------------------------------------- suite

cmd_suite() {
  local cop cla status passed failed ignored lint
  begin suite
  cop="$(copilot_bin)"
  cla="$(claude_bin)"
  [[ -n "$cop" && -n "$cla" ]] ||
    end "NOT RUN" "the suite needs both host binaries (copilot: ${cop:-missing}, claude: ${cla:-missing}); see run.sh doctor"
  [[ "$(free_gb)" -ge 6 ]] || end "NOT RUN" "only $(free_gb) GB of disk is free, and compiling the tests needs about 6 GB"
  build
  cd "$ROOT" || exit 2
  say "running every workspace test with both hosts required (10 to 30 minutes)"
  # No account is used: the hosts run offline against scripted models.
  env -u INTERLOCK_DB -u INTERLOCK_ATTEMPT -u INTERLOCK_TOKEN -u INTERLOCK_SESSION -u INTERLOCK_PROFILE \
    -u COPILOT_GITHUB_TOKEN -u GH_TOKEN -u GITHUB_TOKEN \
    INTERLOCK_REQUIRE_HOSTS=1 INTERLOCK_COPILOT_BIN="$cop" INTERLOCK_CLAUDE_BIN="$cla" \
    cargo test --workspace --locked > "$OUT/cargo-test.txt" 2>&1
  status=$?
  read -r passed failed ignored < <(awk '/^test result:/ {
      for (i = 1; i < NF; i++) {
        if ($(i + 1) ~ /^passed/) p += $i
        if ($(i + 1) ~ /^failed/) f += $i
        if ($(i + 1) ~ /^ignored/) g += $i
      }
    } END { print p + 0, f + 0, g + 0 }' "$OUT/cargo-test.txt")
  say "tests: $passed passed, $failed failed, $ignored ignored"
  python3 eval/test_harness.py > "$OUT/harness-tests.txt" 2>&1
  local harness=$?
  cargo clippy --workspace --all-targets --locked > "$OUT/clippy.txt" 2>&1
  lint="clippy $(grep -c '^warning' "$OUT/clippy.txt") warnings"
  if cargo fmt --all --check > "$OUT/fmt.txt" 2>&1; then lint="$lint, format clean"; else lint="$lint, format differs"; fi
  local summary harness_said=pass
  [[ $harness == 0 ]] || harness_said=FAIL
  summary="$passed passed, $failed failed, $ignored ignored (352, 0 and 2 on Linux and 353, 0 and 2 on macOS when this kit was made); evaluation harness tests $harness_said; $lint"
  [[ $status == 0 && $failed == 0 && $passed -gt 0 && $harness == 0 ]] || end FAIL "$summary; see cargo-test.txt and harness-tests.txt"
  end PASS "$summary"
}

# ---------------------------------------------------------------- github

cmd_github() {
  local repo="${INTERLOCK_LIVE_GITHUB_REPO:-}" status pr
  begin github
  [[ "$repo" == */* ]] ||
    end "NOT RUN" "set INTERLOCK_LIVE_GITHUB_REPO=<owner>/<test-repo>, a separate repository made for this test"
  [[ "$(lower "$repo")" != "$(lower "$THIS_REPOSITORY")" ]] ||
    end "NOT RUN" "INTERLOCK_LIVE_GITHUB_REPO must name a separate test repository, not $THIS_REPOSITORY"
  command -v gh > /dev/null || end "NOT RUN" "gh is not installed"
  gh auth status > "$OUT/gh-auth.txt" 2>&1 || end "NOT RUN" "gh is not signed in, or GitHub refused its token: export GH_TOKEN (TESTING.md, step 3); see gh-auth.txt"
  gh api "repos/$repo" --jq '{full_name, default_branch, visibility}' > "$OUT/repository.json" 2> "$OUT/repository.err" ||
    end "NOT RUN" "this token cannot read $repo: $(head -c 300 "$OUT/repository.err")"
  gh api "repos/$repo/commits?per_page=1" > /dev/null 2>&1 ||
    end "NOT RUN" "$repo has no commits yet: create it with a README, so it has a default branch"
  cd "$ROOT" || exit 2
  say "delivering a verified fix to $repo (a few minutes; it opens and merges a pull request there)"
  # git pushes with gh's credentials, without changing the global git configuration.
  GIT_CONFIG_COUNT=2 \
    GIT_CONFIG_KEY_0=credential.https://github.com.helper GIT_CONFIG_VALUE_0='' \
    GIT_CONFIG_KEY_1=credential.https://github.com.helper GIT_CONFIG_VALUE_1='!gh auth git-credential' \
    INTERLOCK_LIVE_OUT="$OUT" \
    cargo test --locked -p interlock-cli --test live_github -- --ignored --nocapture > "$OUT/cargo-test.txt" 2>&1
  status=$?
  pr="$(json "$OUT/summary.json" '", ".join("pull request #%s %s" % (p["number"], p["state"]) for p in d["pull_request"])')"
  if [[ $status == 0 ]] && grep -q "^test result: ok. 1 passed" "$OUT/cargo-test.txt"; then
    end PASS "delivered to $repo through G5 and G6 (${pr:-see summary.json}); the base branch holds exactly the verified tree"
  fi
  end FAIL "live delivery failed (${pr:-no pull request recorded}); see cargo-test.txt and summary.json"
}

# ---------------------------------------------------------------- copilot

cmd_copilot() {
  local host=copilot model="" status state sig
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --model) model="${2:?--model needs a value}" && shift 2 ;;
      --host) host="${2:?--host needs a value}" && shift 2 ;;
      *) usage_error "copilot: unknown option $1" ;;
    esac
  done
  begin "$host-run"
  [[ "$host" != copilot ]] || [[ -n "$(copilot_bin)" ]] || end "NOT RUN" "Copilot CLI is not installed"
  [[ "$host" != copilot ]] || refuse_offline_copilot
  build
  example_repo "$host-run"
  echo "$WORK" > "$OUT/WORKDIR"
  il init init || end FAIL "interlock init failed: $(cat "$OUT/init.err")"
  if [[ "$host" == copilot ]]; then
    copilot_signed_in
  else
    il host-inspect host inspect --host "$host" --save
  fi
  il task-create task create task.toml || end FAIL "task create failed: $(cat "$OUT/task-create.err")"
  say "running export-retry on $host: a worker session, then a verifier session (a few minutes)"
  "$BIN" run export-retry --host "$host" ${model:+--model "$model"} > "$OUT/run.json" 2> "$OUT/run.err"
  status=$?
  collect_task export-retry
  state="$(task_state)"
  sig="$(signals)"
  if [[ $status == 0 && "$state" == "done" ]]; then
    end PASS "export-retry is done on $host's real model, moves $sig; the fix is in verified-output.diff"
  fi
  end FAIL "export-retry ended ${state:-unknown} on $host (exit $status, moves: ${sig:-none}); see run.err, status.json and transitions.json"
}

# ---------------------------------------------------------------- guided

cmd_guided() {
  begin guided
  [[ -n "$(copilot_bin)" ]] || end "NOT RUN" "Copilot CLI is not installed"
  refuse_offline_copilot
  build
  example_repo guided
  echo "$WORK" > "$OUT/WORKDIR"
  touch "$OUT/STARTED"
  il init init || end FAIL "interlock init failed: $(cat "$OUT/init.err")"
  copilot_signed_in
  il setup setup --host copilot || end FAIL "interlock setup failed: $(cat "$OUT/setup.err")"
  cat << EOF | tee "$OUT/INSTRUCTIONS.txt"

The repository is ready: $WORK
It holds the export-retry bug (examples/export-retry/README.md) and its task
file, task.toml. interlock's skills are in .github/skills/ and its hooks in
.interlock/guided/copilot-plugin.

1. Start Copilot there, with interlock on its PATH:

     export PATH="$ROOT/target/release:\$PATH"
     cd $WORK
     copilot --plugin-dir .interlock/guided/copilot-plugin

2. Type an ordinary request, without naming a skill:

     Fix the bug where export writes rows twice after a transient failure. The task is described in task.toml.

   Approve what Copilot asks to run. Note whether it uses the interlock
   skills (it says "skill" and runs \`interlock\` commands) or just edits
   exporter.py.

3. If it did not use them, type:

     /interlock-route Fix task export-retry, described in task.toml

4. When Copilot says the task is done, or stops, leave with /exit and run:

     $ROOT/testkit/run.sh guided-collect
EOF
  redact "$OUT"
  printf 'RESULT: NOT RUN: waiting for the guided session; then run testkit/run.sh guided-collect\n' | tee "$OUT/RESULT.txt"
}

cmd_guided_collect() {
  local guided="" state sig skills d
  for d in "$RESULTS"/guided-*; do [[ -f "$d/WORKDIR" ]] && guided="$d"; done
  [[ -n "$guided" && -f "$guided/WORKDIR" ]] || usage_error "no guided session to collect: run testkit/run.sh guided first"
  OUT="$guided"
  WORK="$(cat "$OUT/WORKDIR")"
  cd "$WORK" || end FAIL "the guided repository is gone: $WORK"
  say "== guided-collect: results in ${OUT#"$ROOT"/}"
  # Copilot's own record of each session since setup, session-state/<id>/events.jsonl:
  # every tool call, skills included.
  local state_dir="${COPILOT_HOME:-$HOME/.copilot}/session-state" events
  mkdir -p "$OUT/copilot-sessions"
  if [[ -d "$state_dir" ]]; then
    while IFS= read -r events; do
      cp "$events" "$OUT/copilot-sessions/$(basename "$(dirname "$events")").jsonl"
    done < <(find "$state_dir" -name events.jsonl -newer "$OUT/STARTED")
  fi
  skills="$(grep -rhoE '\\?"skill\\?"[[:space:]]*:[[:space:]]*\\?"interlock-[a-z]+' "$OUT/copilot-sessions" 2> /dev/null |
    grep -oE 'interlock-[a-z]+' | sort | uniq -c | awk '{printf "%s%s x%s", sep, $2, $1; sep=", "}')"
  echo "${skills:-none recorded}" > "$OUT/skills-invoked.txt"
  if ! "$BIN" status export-retry > /dev/null 2>&1; then
    end FAIL "the session never created task export-retry; skills invoked: ${skills:-none recorded}"
  fi
  collect_task export-retry
  state="$(task_state)"
  sig="$(signals)"
  local binding
  binding="$(json "$OUT/attempts.json" '", ".join(sorted({a["role"] + ": " + a["binding"]["via"] for a in d}))')"
  if [[ "$state" == "done" ]]; then
    end PASS "export-retry is done in a guided Copilot session, moves $sig; skills invoked: ${skills:-none recorded}; attempts bound as $binding"
  fi
  end FAIL "export-retry ended ${state:-unknown} (moves: ${sig:-none}); skills invoked: ${skills:-none recorded}"
}

# ---------------------------------------------------------------- eval

cmd_eval() {
  local model="" repeats=1 fake=0 conditions="plain,skills,interlock" tasks=("${HARDER_TASKS[@]}") status
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --model) model="${2:?--model needs a value}" && shift 2 ;;
      --repeats) repeats="${2:?--repeats needs a value}" && shift 2 ;;
      --conditions) conditions="${2:?--conditions needs a value}" && shift 2 ;;
      --all-tasks) tasks=() && shift ;;
      --tasks)
        shift
        tasks=()
        while [[ $# -gt 0 && "$1" != --* ]]; do tasks+=("$1") && shift; done
        ;;
      --fake-model) fake=1 && shift ;;
      -h | --help)
        cat << 'EOF'
run.sh eval [--model M] [--repeats N] [--conditions plain,skills,interlock]
            [--tasks T ... | --all-tasks] [--fake-model]

Runs the three conditions on Copilot CLI with its real model. By default:
the four tasks interlock did worst on (go-dotted-section go-duration-days
py-thousands py-date-filter), one repeat, so 12 runs. --all-tasks runs all
eleven stand-in tasks, or your own S3 tasks once they are in
eval/taskset/tasks (TESTING.md, step 8). --fake-model runs Copilot offline
against the scripted model: plumbing only, not evaluation evidence.
EOF
        exit 0
        ;;
      *) usage_error "eval: unknown option $1" ;;
    esac
  done
  begin "eval-copilot$([[ $fake == 1 ]] && echo -fake-model)"
  local cop
  cop="$(copilot_bin)"
  [[ -n "$cop" ]] || end "NOT RUN" "Copilot CLI is not installed"
  command -v go > /dev/null || end "NOT RUN" "Go is not installed (the Go tasks need it)"
  build
  if [[ $fake == 0 ]]; then
    refuse_offline_copilot
    example_repo eval-auth
    il init init
    copilot_signed_in
  fi
  cd "$ROOT" || exit 2
  local H=eval/harness.py state="$RESULTS/eval-state" runs
  runs="$(mktemp -d "${TMPDIR:-/tmp}/interlock-eval-runs-XXXXXX")"
  say "building the frozen task set and checking it (no model calls)"
  python3 "$H" build --state-dir "$state" > "$OUT/taskset-build.txt" 2>&1 || end FAIL "the task set does not match its lock: see taskset-build.txt"
  python3 "$H" selftest --state-dir "$state" --interlock-bin "$BIN" --out "$OUT/selftest.json" > "$OUT/selftest.txt" 2>&1 ||
    end FAIL "the task set's self-test failed: see selftest.txt"
  say "running $conditions on ${tasks[*]:-all tasks}, $repeats repeat(s), on Copilot $([[ $fake == 1 ]] && echo '(scripted model)' || echo '(real model)')"
  # Copilot reports premium requests, not dollars, so the harness charges each
  # session a fixed reserve against --budget-usd; the cap here only stops a
  # runaway. The task list and repeats bound the run.
  local extra=()
  [[ -n "$model" ]] && extra+=(--model "$model")
  [[ $fake == 1 ]] && extra+=(--fake-model)
  [[ ${#tasks[@]} -gt 0 ]] && extra+=(--tasks "${tasks[@]}")
  python3 "$H" run --host copilot --conditions "$conditions" \
    --skills-generate --interlock-skills --budget-usd 1000 --repeats "$repeats" \
    --state-dir "$state" --results "$OUT/results" --run-base "$runs" --label main \
    --interlock-bin "$BIN" --copilot-bin "$cop" ${extra[@]+"${extra[@]}"} > "$OUT/run.txt" 2>&1
  status=$?
  rmdir "$runs" 2> /dev/null
  python3 "$H" report --results "$OUT/results" > "$OUT/report.txt" 2>&1
  python3 "$H" export --results "$OUT/results" --dest "$OUT/export" > "$OUT/export.txt" 2>&1
  local accepted
  accepted="$(json "$OUT/results/report.json" '"; ".join("%s %s/%s accepted, %s invoked a skill" % (c, v["accepted"], v["runs"], v["runs_invoking_skills"]) for c, v in d["all_runs"]["conditions"].items())')"
  [[ $status == 0 && -f "$OUT/results/report.md" ]] || end FAIL "the evaluation did not finish (exit $status); see run.txt"
  end PASS "the evaluation ran: ${accepted:-see results/report.md}. Accepted counts are findings, not a pass or fail"
}

# ---------------------------------------------------------------- netfs

# The mount point holding a directory, and that filesystem's type, as the
# operating system names it (statfs on Linux, mount(8) on macOS).
mount_point() { df -P "$1" | awk 'NR == 2 { print $NF }'; }
fs_type() {
  if [[ "$(uname -s)" == Linux ]]; then
    stat -f -c %T "$1" 2> /dev/null
  else
    mount | grep -F " on $(mount_point "$1") (" | sed -e 's/.* (//' -e 's/[,)].*//'
  fi
}

cmd_netfs() {
  local dir="${1:-}" probe status
  [[ -n "$dir" && -d "$dir" ]] || usage_error "netfs needs a directory on a network filesystem (NFS, SMB, sshfs)"
  begin netfs
  build
  {
    echo "path: $dir"
    echo "filesystem type: $(fs_type "$dir")"
    df -P "$dir"
    findmnt -T "$dir" -o TARGET,SOURCE,FSTYPE 2> /dev/null || mount | grep -F " on $(mount_point "$dir") (" || true
  } > "$OUT/mount.txt" 2>&1
  probe="$(mktemp -d "$dir/interlock-netfs-probe-XXXXXX")" || end "NOT RUN" "cannot write in $dir"
  (cd "$probe" && git init -q && "$BIN" init) > "$OUT/init.json" 2> "$OUT/init.err"
  status=$?
  (cd "$probe" && INTERLOCK_ALLOW_NETWORK_FS=1 "$BIN" init) > "$OUT/init-override.json" 2> "$OUT/init-override.err"
  echo "with INTERLOCK_ALLOW_NETWORK_FS=1: exit $?" >> "$OUT/mount.txt"
  rm -rf "${probe:?}"
  local fs
  fs="$(fs_type "$dir")"
  if [[ $status == 2 ]] && grep -q network_filesystem "$OUT/init.err" "$OUT/init.json"; then
    end PASS "interlock refused a store on $dir ($fs): $(head -c 300 "$OUT/init.err")"
  fi
  end FAIL "interlock opened a store on $dir ($fs, exit $status). If that is a network filesystem, the check missed it; see mount.txt"
}

# ---------------------------------------------------------------- pack

cmd_pack() {
  [[ -d "$RESULTS" ]] || usage_error "nothing to pack: $RESULTS does not exist"
  local zipfile
  zipfile="$ROOT/testkit-results-$(stamp).zip"
  cmd_doctor > "$RESULTS/doctor.txt" 2>&1
  versions > "$RESULTS/versions.txt"
  redact "$RESULTS"
  # eval-state is the harness's working copy of the task set: rebuilt on demand, never evidence.
  python3 -I - "$RESULTS" "$zipfile" << 'ZIP' || exit 1
import os, sys, zipfile
root, dest = sys.argv[1], sys.argv[2]
top = os.path.basename(root.rstrip("/"))
with zipfile.ZipFile(dest, "w", zipfile.ZIP_DEFLATED) as z:
    for d, dirs, files in os.walk(root):
        if d == root and "eval-state" in dirs:
            dirs.remove("eval-state")
        for f in files:
            p = os.path.join(d, f)
            z.write(p, os.path.join(top, os.path.relpath(p, root)))
ZIP
  say "packed: $zipfile ($(du -h "$zipfile" | cut -f1))"
  say "credential values were removed from it; check REDACTED.txt files if any were found"
  [[ -f /.dockerenv ]] && say "from your own machine: docker cp interlock-testkit:$zipfile ."
  return 0
}

case "${1:-}" in
  doctor) cmd_doctor ;;
  suite) cmd_suite ;;
  github) cmd_github ;;
  copilot) shift && cmd_copilot "$@" ;;
  guided) cmd_guided ;;
  guided-collect) cmd_guided_collect ;;
  eval) shift && cmd_eval "$@" ;;
  netfs) shift && cmd_netfs "$@" ;;
  pack) cmd_pack ;;
  -h | --help | help) usage ;;
  *) usage_error "unknown command: ${1:-none}" ;;
esac
