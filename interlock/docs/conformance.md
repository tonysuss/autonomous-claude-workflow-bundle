# Conformance to design draft 1

An independent agent, new to the project, checked the build against the design item by item at commit `68a3e86`. It enumerated 165 requirements from the design itself, read the code for each, and ran 42 of the cited tests one at a time with `INTERLOCK_REQUIRE_HOSTS=1`. It found 114 met, 4 met but untested, 25 partial, 14 deviating, 4 not met and 4 not verifiable here. Its report follows unchanged, after the list of what changed because of it.

## Changed since the audit

| Row | Was | Now | What changed | Covering test |
| --- | --- | --- | --- | --- |
| 1.3.13, 1.5.9, 1.8.2 | partial | met | Operator commands (`grant`, `task unblock/tree/retry/fail/cancel`, `integrate`, `reconcile`, `run`, `check run --operator`) refuse to run when the caller's own environment names an attempt or session, or any ancestor process carries the session marker. The audit's bypass, `env -u … $I grant create`, is refused | `run_fake_host::a_session_cannot_grant_itself_authority_however_it_calls_interlock`; removing the ancestry walk makes it fail (the grant is created) |
| 1.7.1 | partial | met | G1 records a hash of untracked, non-ignored, non-generated inputs (`git::untracked_tree`), in `interlock run` and `interlock task ready` | `git::tests::the_untracked_part_of_the_snapshot_is_hashed_and_generated_files_are_not` |
| 1.7.2 | partial | met | G2 refuses when the effective grant lacks, or denies outright, a tool the workflow's role needs | `lifecycle::g2_refuses_when_the_host_policy_takes_away_a_tool_the_role_needs`; end to end in `run_fake_host::the_host_policy_narrows_every_session_and_a_tool_the_worker_needs_refuses_g2` |
| 1.8.6, 1.9.12 | partial | met | The host side of the effective grant comes from `[host_policy.<host>]` in `.interlock/config.toml` (denied tools, allowed classes) instead of being always open | `run_fake_host::the_host_policy_narrows_every_session_and_a_tool_the_worker_needs_refuses_g2` |
| 1.5.4, 1.10.12 | partial | met on Claude Code; Copilot reports no model | Each attempt's `spent.model` records the model the host reported (Claude Code's init event) | `claude_code::tests::summarizes_the_result_event` |
| 1.5.12 | partial | met | The brief lists each upstream task's state and last accepted result | `sessions::a_brief_carries_what_upstream_tasks_produced` |
| 1.12.20 | partial | met | Capabilities a real adapter finds missing in a host's help give the declared fallback (recorded in the G2 reason) or block the task before any session | `run_fake_host::capabilities_the_adapter_finds_missing_give_the_declared_fallback_or_block` |
| 1.10.7 | met-untested | met | The Copilot plan test asserts `--model` | `copilot::tests::plans_a_session` |
| Spot check 2 | did not compile alone | passes | `cargo test -p interlock-schema --test conformance` builds with `validate` on | that command |
| Section 3 | | corrected | README: S2 and the CLI-subprocess adapter, tokio, scope under the permissive profile and the host policy listed as changes; all six §14 questions; "live" now says scripted model; the regression-test claim names its two exceptions; harder-task wording; single-run tasks, schemas v0 and example-based guard tests listed as not proven. Forge evidence: 16 mutations, 15 caught. Evaluation evidence: the two spend figures reconciled, the two leak flags marked as false positives | |
| 1.4.2 | partial | met | The lifecycle guards are property-tested against an oracle. Each case drives a random sequence of moves through the pure API (G1 to G7, R1 to R3, block and unblock, fail, cancel, results from current and late attempts at any epoch, evidence from either role on current and stale trees, forge reports of the pinned head, of a prefix of it or of another head, new trees), and checks every call against what the design's guard list says it must do in the task's state, given its evidence, budget and integration, so a guard that stops firing fails as surely as one that fires wrongly. After every move it also checks the design's invariants: only guard-list transitions; the epoch never decreases and a stale result is never applied; done only through G7, or G5 then G6, after G4, on current passing evidence, and G6 lands only the pinned head of the task's current tree; G7 never fires on a task that must integrate; terminal states are never left; unblock returns to the exact state with its work; attempts stay within the budget, a spent budget fails rather than sends back, and a ready task holds no attempt. The moves are weighted to carry tasks to verified and integrating and then make their evidence stale (a new tree, a failed assessment, a rebuilt tree) before advancing or confirming. A second test runs 512 fixed sequences and requires each of 35 branches to be met at least 20 times, among them G6 blocked on stale evidence (the rarest, 29) and on a changed tree (41), G6 refused for another head (36), G7 withheld from a task that must integrate (242), R2 from integrating on stale evidence (94) and on a refused merge (49), and R1 and R3 with the budget spent (181, 36). Both tests run in about 2.4 s. Breaking each of 33 checks in turn, in a copy of the workspace with proptest's saved failures deleted before every run, the property catches 31 on all 5 seeds and 2 on none ([script and log](../evidence/core/lifecycle-mutations/)); the deterministic coverage test fails for the same 31. The two guard states no sequence of moves reaches: G2's budget check (R1 and R3 fail a task rather than leave it ready with its budget spent) and G3's current-attempt check (every G2 opens its attempt at a new epoch, so the epoch check refuses any other attempt first). Each has an example test that fails with its check removed. The audit-response version of this row said 12 of 14 were caught: its script kept proptest's saved failures between mutations, so a case found for one mutation was replayed against the next, and its property checked invariants only, without the oracle | `lifecycle_props::the_guards_hold_over_any_sequence_of_moves` (512 cases); `::the_sequences_reach_every_branch_of_every_guard`; `lifecycle::g2_refuses_once_the_attempt_budget_is_spent`, `::g3_supersedes_another_attempt_at_the_current_epoch` |
| 1.7.6 | met | met, stricter | G6 is open to an operator through `interlock integrate confirm`, which reports the forge's answer by hand, so the policy core's G6 now makes the checks `settle_operation` makes. The property test found two gaps: an empty or short merged head matched any pinned head as its prefix, and G6 never read the evidence, so a merge confirmed after a tree was recorded while integrating, or after a later failed assessment, reached done. Review found a third: once the new tree was verified, a merge of the old pinned head confirmed by hand landed on the new tree's evidence. Now every operation records the task's tree when it is planned (`intent.tree`, schema v1), and G6 needs the pinned head named by at least seven characters (in either case of hex digits), that tree still to be the task's, and current passing evidence; a merge of the pinned head without them blocks the task at integrating with the reason. `integrate confirm` also refuses an operation already confirmed or failed, and a merge it reports when the task has no landing authority blocks the task, as settling does | `lifecycle::g6_needs_the_pinned_head_named_by_at_least_seven_characters`, `::a_merge_confirmed_after_the_evidence_went_stale_or_failed_is_not_g6`, `::a_merge_of_the_pinned_head_lands_only_while_its_tree_is_the_tasks`; `operations::a_merge_confirmed_by_hand_never_lands_a_short_head_or_a_changed_tree`, `::a_hand_confirmed_merge_of_the_old_head_never_lands_a_newer_tree`, `::a_hand_confirmed_merge_refuses_an_operation_already_settled`, `::a_hand_confirmed_merge_needs_landing_authority_when_it_is_reported`; each fails with its fix undone. In the property: `G6-tree-unchecked`, `G6-case-sensitive` and `G6-short-prefix` are caught on every seed |
| 1.5.1, 1.12.9 | partial | met | Schemas v1: every `$id` moves to `https://schemas.interlock.dev/v1/`; [schemas/CHANGELOG.md](../schemas/CHANGELOG.md) lists what v1 adds over the S4 draft (only optional fields, enum values and the check-run record). Records in the store carry no `$id` and the store's migrations are unchanged, so nothing needs migrating; a store written by the last v0 build (a2ec7b9) is kept as a fixture | `conformance::every_schema_names_itself_under_v1`; `v0_store::every_record_the_v0_build_wrote_conforms_to_v1_as_stored`, `::a_v0_store_reads_unchanged_and_its_evidence_still_counts`, `::a_v0_store_takes_new_moves` |
| 1.3.5, 1.15.4 | partial; met | met on Linux | Opening the store refuses a directory on a network filesystem: exit 2, `network_filesystem`; `INTERLOCK_ALLOW_NETWORK_FS=1` overrides it ([docs/runtime.md](runtime.md)). On Linux it reads the `statfs` magic number (NFS, SMB, SMB2, CIFS, AFS, Coda, NCP, Ceph, Lustre, GPFS, BeeGFS, OrangeFS, OCFS2, GFS2) and, for FUSE, the mount's type in `/proc/self/mountinfo` (`fuse.sshfs`, `fuse.rclone`, `fuse.gvfsd-fuse`, `fuse.mfs`, `fuse.lizardfs`, `fuse.s3ql`, `fuse.davfs`, `fuse.keybase`, `fuse.fuse-nfs` and others). A FUSE layer whose mount source is a directory (gocryptfs, bindfs) is judged by that directory too, up to four layers; a layer whose source is not a path (encfs) is not followed. On macOS it reads the type name `statfs` gives (`nfs`, `smbfs`, `afpfs`, `webdav`, `ftp`, `cifs`); macFUSE reports one type for every FUSE filesystem, so sshfs there is not caught, and this path was compiled for macOS but has not run on it. Elsewhere nothing is checked. No network mount can be made here: a refusal of a real NFS or SMB mount is not observed. Locally served FUSE mounts typed `fuse.sshfs`, and `fuse.gocryptfs` over it, were refused, and one typed `fuse.fuse-overlayfs` was not ([evidence](../evidence/runtime/README.md#8-no-store-on-a-network-filesystem)) | `netfs::tests::*` (magic numbers, FUSE types, the mount holding a path, layers, the refusal and the override, a local directory); `cli::a_store_on_local_disk_opens_without_the_network_override` |
| 1.10.8 | partial | met | Every headless Copilot session (`interlock run`, `interlock verify`) runs as `copilot -p --agent interlock-hooks:interlock-<role>`: the adapter writes the role's agent profile into the run's hooks plugin, with the role's instructions (now in the system prompt rather than leading the prompt) and, as its `tools`, what the attempt's grant allows and does not deny outright. Every `--allow-tool` and `--deny-tool` flag is kept. Copilot adds its own `skill` and `sql` to every agent's tools. So the verifier is never offered an edit tool, and neither role is offered `task`, which no workflow grants and Copilot's flags do not gate; a call to a tool the agent lacks fails as "Tool 'edit' does not exist." and is counted as a denial; the profile's list stands in for the available set that `--available-tools` would give, which 1.10.6 noted interlock never passed. Agent names must be lowercase letters, digits and hyphens. These are top-level agents interlock starts, not the design's sub-agent delegation (1.10.4 is unchanged). Findings on Copilot 1.0.91 are in [docs/host-spike-2026-10-05.md](host-spike-2026-10-05.md) | `run_copilot::each_session_runs_as_interlocks_agent_for_its_role_and_the_tool_filters_hold` (the agent each session ran, from Copilot's own session log; the instructions in the system prompt; the tools offered; denials); `run_copilot::copilot_keeps_the_deny_rules_for_a_session_run_as_an_agent`; `guided_copilot::a_bug_fix_runs_end_to_end_in_guidance_mode` (the verifier, offered no edit tool, has its shell commit denied by a rule or the hook and its shell write into the store denied by the hook); `copilot::tests::plans_a_session_as_interlocks_agent_for_its_role` (and refuses bad agent names), `::an_agent_lists_only_the_tools_its_grant_leaves_it`, `::summarizes_jsonl_output` (missing tools counted as denials) |
| Section 3, item 14 | | corrected | The README's "Custom agents: `--agent`" for Copilot now describes what interlock does | |

Still open, as the audit found them: S2 (1.12.2, 1.15.2), with the P0 adapter decision now recorded as "keep the CLI adapter for v1" in the README (1.1.4, 1.4.8, 1.4.9, 1.10.2, 1.12.6 stay deviations); Copilot with a real model (1.1.1, 1.12.10); the S3 baseline and repeated runs (1.12.3, 1.12.7, 1.12.32, 1.13.4); live GitHub.

## The audit, as written

Audited build: `interlock`, branch `interlock-foundation`, commit `68a3e86`. No file under `crates/`, `schemas/`, `skills/` or `agents/` changed between `858879e` (the build the evidence was recorded on) and `68a3e86` (`git diff --stat 858879e..68a3e86 -- crates schemas skills agents` is empty).

Design: `scratchpad/design.txt` (606 lines), including the guard list at its end (lines 594-606).

Method: I read the design and enumerated its items from the design, not from the README. I then read the code that implements each item, ran 42 cited tests one at a time plus the harness's own unit tests (section 2), checked 12 evidence-README claims against their raw files, and ran three probes against the built binary in a scratch repository. I made no live model calls and edited nothing in the repository.

Paths below are relative to `interlock/` unless they start with `/`. "Fake gh" means `crates/interlock-forge/src/fake_gh.py` with a local bare repository. "Offline Copilot" means the real Copilot CLI 1.0.91 driven by a scripted model on localhost.

Status key: `met` = implemented, with a test or recorded run that would fail if it broke. `met-untested` = implemented, with no such test. `partial` = some of it is missing. `deviates` = the build does something different. `not met`. `not verifiable here` = the evidence it needs cannot be produced in this container.

---

## 1. Conformance matrix

### 1.1 Settled decisions (§1)

| # | Design item | Status | Evidence | Notes |
|---|---|---|---|---|
| 1.1.1 | §1 Host: GitHub Copilot CLI only, pinned; the claim is "validated on Copilot only" | deviates | `crates/interlock-adapter/src/lib.rs:159` registers `Copilot` and `ClaudeCode`; README.md:7-12; every live-model run is on Claude Code (`evidence/skills/p1/claude-code-*`, `evidence/runtime/live-claude-reattach*`, `evidence/forge/live-claude-code`, `evidence/evaluation/claude-code-v3`) | Documented in README "Hosts" and "What is not proven", and in docs/skills.md "Changes from the design". Partly defensible, because Copilot could not sign in here (`evidence/forge/gh-auth-status.txt`, GH_TOKEN invalid). The design's one validation claim, "validated on Copilot", cannot be made: Copilot ran only against a scripted model. |
| 1.1.2 | §1 Core: Rust, one static binary, bundled SQLite holding state, events and acks in transactions; larger artifacts in files | partial | `Cargo.toml` `rusqlite = { features = ["bundled"] }`; `crates/interlock-store/src/lib.rs:510` (one IMMEDIATE transaction per change); content-addressed check output, test `interlock-store/tests/invariants.rs::check_output_is_kept_as_a_content_addressed_artifact` | `ldd target/debug/interlock` shows dynamic links to libc, libm and libgcc_s. There is no musl or `crt-static` configuration, so the binary is not static. README does not claim it is. |
| 1.1.3 | §1 Records: JSON Schema is the source of truth; Rust types generated from it | deviates | `crates/interlock-schema/src/lib.rs:1-5` (types written by hand); `crates/interlock-schema/src/validate.rs`; `crates/interlock-schema/tests/conformance.rs::every_record_kind_round_trips_through_its_schema` | Documented in README "Changes" (typify is alpha). Defensible: the store validates every write and a conformance test catches drift. That test does not compile under `cargo test -p interlock-schema --test conformance` (E0425 `Validators`) unless `--features validate` is passed (see 2.1). |
| 1.1.4 | §1 Copilot integration: the official Rust Copilot SDK if S2 confirms parity, otherwise a thin Node adapter process behind the same contract | deviates | `crates/interlock-adapter/src/copilot.rs:72-110` builds a `copilot -p … --output-format json` subprocess; `grep -ri 'sdk\|json-rpc\|interlock-copilot' README.md docs evidence/*/README.md crates/*/src` finds nothing | **Undocumented.** The build uses neither option the design names: it shells out to the CLI in prompt mode. S2 was never run (1.12.2). |
| 1.1.5 | §1 Skills: one host-neutral canonical source; a generator emits Copilot files and plain Agent Skills output | met | `skills/*/skill.toml`, `skills/*/SKILL.md`; `crates/interlock-skillgen/src/lib.rs:509` `generate`; tests `interlock-skillgen/tests/generate.rs::every_target_validates`, `::copilot_gets_prefixed_github_skills_routed_references_and_no_custom_agent`, `::plain_agent_skills_keep_the_intent_in_metadata_and_validate_on_disk` (9/9 pass, 2.1) | The build adds a third target, a Claude Code plugin. |
| 1.1.6 | §1 Reuse patterns, not code; keep the MIT notice | met | `skills/NOTICE`; `generate.rs::the_built_in_catalog_is_the_source_tree` asserts the notice contains "Lauren Tan" and "MIT" | |
| 1.1.7 | §1 Not in v1: Graphite, Slack/Benny, style rules, multi-day programs, hosted multi-controller | met-untested | `grep -rni 'graphite\|slack' crates` finds nothing | The requirement is an absence, so no test is expected. |

### 1.2 Review gaps closed (§2)

| # | Design item | Status | Evidence | Notes |
|---|---|---|---|---|
| 1.2.1 | §2 Orchestrator patterns: re-attach by run ID, mark orphans, write the handoff first, deduplicate kickoffs, classify failures, plus lease epochs | met | `crates/interlock-supervisor/src/run.rs:403` `restart_reconcile`, `:484` `reattach`; `crates/interlock-store/src/lib.rs:806` `record_handoff` refuses a second handoff; `EndReason` (`records.rs:284`); tests `run_robustness::a_supervisor_killed_mid_session_reattaches_and_carries_on` (pass, 2.1), `interlock-store/tests/sessions.rs::an_attempt_gets_one_session_and_its_handoff_is_written_first` | |
| 1.2.2 | §2 Action classes with a pause class nobody can override, time-bound grants, a landing-authority field, and durable records instead of the transcript for resuming | met | `crates/interlock-core/src/grants.rs:60,75,157`; `crates/interlock-core/src/brief.rs:49`; `crates/interlock-supervisor/src/export.rs:39` | |
| 1.2.3 | §2 The merge passes the verified head SHA (`--match-head-commit`) and every merge is reconciled as an operation | met | `crates/interlock-forge/src/gh.rs:267`; `evidence/forge/fault-tests/invariant-7-crash-then-reconcile/gh-calls.json` (one `pr merge` call, carrying `--match-head-commit`) | Tested against the fake gh only. Live GitHub: see 1.16.1. |
| 1.2.4 | §2 Canonical skills declare an invocation intent, the generator emits host fields, and a load test proves every skill loads | met | `skill.toml` `invocation =`; `crates/interlock-skillgen/src/lib.rs:532-536`; `crates/interlock-cli/tests/skills_load.rs::every_generated_skill_loads_on_copilot`, `::every_model_invoked_skill_reaches_copilots_model_through_the_skill_tool` (both pass, 2.1) | |

### 1.3 Architecture and command surface (§3)

| # | Design item | Status | Evidence | Notes |
|---|---|---|---|---|
| 1.3.1 | §3 Two paths, interactive and headless, share one core | met | `crates/interlock-core/src/workflow.rs:13` `Mode::{Interactive,Headless}`; both paths call `Store` | |
| 1.3.2 | §3 Interactive path: you work in a Copilot session, generated skills call interlock at each step, and the host agent hands work to custom agents that report back with their attempt token | partial | `crates/interlock-cli/tests/guided_copilot.rs::an_investigation_runs_end_to_end_in_guidance_mode` (pass, 2.1) and `::a_bug_fix_runs_end_to_end_in_guidance_mode`, both with a scripted model; live Claude Code: `evidence/skills/p1/claude-code-bug-fix/` (checked in 2.2) | On Copilot no custom agent is used. `interlock verify` launches a headless verifier instead, and the verifier keeps its token to itself. Both choices are documented in docs/skills.md "Changes from the design". With a real model the path is shown on Claude Code only. |
| 1.3.3 | §3 Headless path: the supervisor starts worker and verifier sessions through the Copilot adapter and collects their results | met | `crates/interlock-supervisor/src/run.rs:251` `run`; `run_copilot.rs::a_bug_fix_runs_to_done_on_copilot` (offline Copilot); live Claude Code runs under `evidence/runtime/live-claude-reattach-integrated/` | On Copilot, shown with a scripted model only. |
| 1.3.4 | §3 Agents only submit claims and results; only interlock writes task state | met | Every move goes through `Store::apply` inside a transaction (`crates/interlock-store/src/lib.rs`); append-only triggers in `migrations/001_initial.sql:103-122` | As the design itself says, an agent with filesystem access could still edit SQLite. The hooks block this by reading command text only. Grants are a weaker spot: see 1.6.2. |
| 1.3.5 | §3 One controller per checkout; no database on a network filesystem | partial | `crates/interlock-supervisor/src/lock.rs:21` (flock); `run_fake_host::only_one_supervisor_gets_the_lock_however_many_race` | Nothing checks for a network filesystem; `grep -rni 'nfs\|statfs' crates` finds nothing. Guided CLI commands do not take the lock; SQLite serializes them instead. |
| 1.3.6 | Command `interlock host inspect`: Copilot version and capabilities | met | `crates/interlock-cli/src/main.rs:1139`; `crates/interlock-adapter/src/copilot.rs:35`; unit tests `copilot.rs::detects_capabilities_from_help`, `::missing_flags_are_not_claimed` | |
| 1.3.7 | Command `interlock task create`: workflow, criteria (input snapshot) | met | `main.rs:865`; `cli.rs::a_bug_fix_runs_from_create_to_done` | The snapshot is recorded at G1 (`task ready`), not at create. |
| 1.3.8 | Command `interlock attempt start`: opens an attempt, bumps the epoch, issues a token | met | `main.rs:930`; `store/src/lib.rs:560`; `invariants.rs::invariant_4_a_late_worker_cannot_advance_the_task` asserts epoch 2 | |
| 1.3.9 | Command `interlock result submit` for the current attempt and epoch | met | `main.rs:997`; `store/src/lib.rs:622` | |
| 1.3.10 | Commands `claim add` / `assess add` against one criterion | met | `main.rs:1026-1029`; `lifecycle.rs:426` | |
| 1.3.11 | Command `interlock status`: state, missing evidence and next moves, as JSON | met | `main.rs:1052`; probe: `status` returns keys `task, evidence, attempts, next_moves` | |
| 1.3.12 | Command `interlock brief` from durable records | met | `main.rs:1059`; `cli.rs::the_brief_is_built_from_records` | Content gaps: see 1.5.12. |
| 1.3.13 | Command `interlock grant`: operator only | partial | `main.rs:1112-1136` has no inside-attempt check. Compare `crates/interlock-cli/src/forge.rs:52` `operator_only`, which guards integrate, reconcile and run only. | **Probe:** in a scratch store, `INTERLOCK_ATTEMPT=att-fake interlock grant create --classes landing --landing coordinator …` succeeds and creates the grant, while `INTERLOCK_ATTEMPT=att-fake interlock reconcile` is refused. The only guard is the hook's text classifier (see 1.6.2). |

### 1.4 Rust workspace (§4)

| # | Design item | Status | Evidence | Notes |
|---|---|---|---|---|
| 1.4.1 | §4 The policy core is pure: no file, network or clock access | met | `grep 'std::fs\|Utc::now\|std::process\|std::net\|std::env' crates/interlock-core/src` finds nothing; its dependencies are chrono, hex, schema, serde, serde_json and sha2 | |
| 1.4.2 | §4 Every guard can be tested with property tests; interlock-core is property-tested | partial | Property tests: `interlock-core/tests/evidence_props.rs` (5), `checks.rs::vacuous_runs_change_nothing`, `delivery.rs::only_the_pinned_head_ever_lands`, `::branch_names_are_injective_and_valid` | The lifecycle guards G1-G7, R1-R3, block, fail and cancel have example-based unit tests only (`interlock-core/tests/lifecycle.rs`). None of them is property-tested. |
| 1.4.3 | interlock-schema: schema files plus Rust types | met | `schemas/*.schema.json` (11 files: 10 records plus common); `crates/interlock-schema/src/lib.rs:67-85` embeds them | Types are written by hand (1.1.3). |
| 1.4.4 | interlock-core: lifecycle, guards, evidence decisions, grant intersection | met | `crates/interlock-core/src/{lifecycle,evidence,grants}.rs` | |
| 1.4.5 | interlock-store: bundled SQLite, migrations, content-addressed artifacts, one transaction per transition | met | `store/src/lib.rs:20-21,490-512`; `invariants.rs::invariant_5_a_failure_mid_apply_leaves_nothing_then_replay_applies_once` (pass, 2.1) | |
| 1.4.6 | interlock-cli: the only entry point for agents, JSON output | met | `crates/interlock-cli/src/main.rs`; `cli.rs::refusals_exit_2_with_a_json_reason` | |
| 1.4.7 | interlock-supervisor: attempts, leases, budgets, timeouts, cancellation, restart reconcile | met | `crates/interlock-supervisor/src/run.rs`; run_robustness suite (5 tests run in 2.1, all pass) | |
| 1.4.8 | interlock-adapter: a capability contract as an in-process trait **and a process protocol**, so a Node adapter can plug in | partial | Trait: `crates/interlock-adapter/src/lib.rs:91` `trait Host` | There is no process protocol, so an out-of-process adapter cannot plug in. Undocumented. |
| 1.4.9 | interlock-copilot: a Copilot adapter on the Rust SDK, gated by S2 | deviates | No such crate (`ls crates`). Copilot lives in `crates/interlock-adapter/src/copilot.rs` as a CLI subprocess | **Undocumented.** The README "Layout" table drops the crate without comment. |
| 1.4.10 | interlock-forge: GitHub operations through gh | met | `crates/interlock-forge/src/{gh,deliver,git}.rs`; forge tests (2.1) | |
| 1.4.11 | interlock-skillgen: canonical to Copilot and plain output; static validation plus load test | met | `crates/interlock-skillgen/src/{lib,validate}.rs`; `skills_load.rs` | |
| 1.4.12 | §4 Library choices to confirm: rusqlite bundled, clap, tokio, serde, typify | deviates | `Cargo.toml` workspace deps: rusqlite (bundled), clap and serde are present; tokio and typify are absent | Dropping typify is documented. Dropping tokio (threads plus signal-hook instead) is not. The design asked only that these be confirmed, so this is minor. |

### 1.5 Records (§5)

| # | Design item | Status | Evidence | Notes |
|---|---|---|---|---|
| 1.5.1 | §5 Every record is defined in JSON Schema and versioned | partial | `schemas/*.schema.json`; every `$id` is `https://schemas.interlock.dev/v0/...` | The schemas exist but stay at v0, though P1 asked for "schemas v1" (1.12.9). Store migrations are versioned (`store/src/lib.rs:20`). |
| 1.5.2 | Task: id, repository, workflow and version, intent, scope, dependencies, criteria, input snapshot, policy digest, budget, state, resume point; state changes only through a guard | met | `crates/interlock-schema/src/records.rs:241-268`; `schemas/task.schema.json`; `conformance.rs::every_record_kind_round_trips_through_its_schema` (passes with `--features validate`) | |
| 1.5.3 | Criterion: id, statement, check reference, minimum strength, required producer; changing it invalidates evidence | met | `records.rs:196-207`; `crates/interlock-core/src/digest.rs:10` (the policy digest covers the criteria); tests `lifecycle.rs::changing_a_criterion_changes_the_policy_digest`, `evidence_props.rs::a_new_policy_digest_invalidates_everything` | No command edits criteria after creation, so the digest is the whole mechanism. |
| 1.5.4 | Attempt: id, task, lease epoch, role, adapter and version, **reported** agent and model, worktree, effective grant, token hash, status; role comes from the attempt | partial | `records.rs:382-410`; role from the attempt at `lifecycle.rs:443,455` | `model` holds the model interlock **asked for** (`run.rs:574`, `self.cfg.model`), not the one the host reports. It is `None` when no `--model` is given, and `SessionSummary` (`adapter/src/session.rs:63`) has no model field. `agent` is interlock's own label (`interlock-worker`), except in guided Claude Code runs. |
| 1.5.5 | Result: attempt, epoch, output tree, changed paths, summary, open questions, accepted or superseded; applied only for the current attempt and epoch | met | `records.rs:476-489`; `lifecycle.rs:373-417`; `invariants.rs::invariant_4_a_late_worker_cannot_advance_the_task` (pass) | Adds a `rejected` status for out-of-scope results. |
| 1.5.6 | Claim: criterion, attempt, strength, currency key, evidence refs, **outcome**; append-only | deviates | `records.rs:494-508` (one `strength` field, no `outcome`); triggers `migrations/001_initial.sql:103-106` | Documented in README "Changes" ("One strength field"). Defensible: the strength scale already includes `blocked` and `failed`. |
| 1.5.7 | Assessment: the same fields, from a verifier attempt; append-only | deviates | `records.rs:494`; separate table `001_initial.sql:50`; triggers `:107-110`; `lifecycle.rs:455` (verifier attempts only) | Same documented deviation. Assessments gain `bound_via`. |
| 1.5.8 | Event: unique id, attempt, epoch, type, payload ref, received at, acknowledged; applied and acknowledged in one transaction | met | `records.rs:543-558`; `store/src/lib.rs:424` `seen_event`, `:431` `put_event` inside the applying transaction; `invariants.rs::invariant_5_a_duplicate_event_is_a_no_op`, `cli.rs::a_crash_mid_apply_loses_nothing_and_the_replay_applies_once` (pass) | |
| 1.5.9 | Grant: principal, task scope, classes and tools, landing authority, origin, expires, revoked; **written only by the operator path** | partial | Fields: `records.rs:561-574`; validation `grants.rs:60` | Every field is present, but the operator-only rule is not enforced (1.3.13, 1.6.2). |
| 1.5.10 | Operation: id, task, kind, intent with expected head SHA, state (planned, started, confirmed, unknown); written before any external call; reconciled on start | met | `records.rs:578-617`; `store/src/operations.rs:91,145,216`; tests `deliver.rs::every_forge_call_finds_its_operation_already_started` (pass), `forge_cli.rs::invariant_7_a_restarted_run_reconciles_before_anything_else` | Adds a `failed` state and the `disarm_auto_merge` kind. |
| 1.5.11 | §5 Currency key: tree (or head and base), check-definition version, environment, policy digest; stale evidence is kept but stops counting | met | `records.rs:164-169`; `evidence.rs:80,98`; `invariants.rs::invariant_1_no_current_evidence_no_pass` (pass); `evidence_props.rs::stale_evidence_never_counts` (pass) | A base move is folded into the tree with `git merge-tree` (docs/forge.md "A changed base…"). |
| 1.5.12 | §5 Resuming never uses a transcript. `brief` rebuilds goal, scope, snapshot, criteria, **upstream results**, checks and the effective grant from records | partial | `crates/interlock-core/src/brief.rs:49-331` | Missing: results of upstream (dependency) tasks. Only this task's last accepted result is included. Of the snapshot, only `base_commit` and `protected` appear, not `repository` or the untracked-input hash. |
| 1.5.13 | §5 pause-safely's `wip:` commit plus resume note becomes the export format | met | `crates/interlock-supervisor/src/export.rs:39,147`; `run_fake_host::pausing_safely_keeps_work_the_worker_committed`, `run_robustness::sigint_cancels_the_session_and_the_exported_work_resumes` (pass, 2.1) | |

### 1.6 Evidence and decisions (§6)

| # | Design item | Status | Evidence | Notes |
|---|---|---|---|---|
| 1.6.1a | §6 A worker pass and a verifier failure never share a row | met | `migrations/001_initial.sql:40-58` (separate `claims` and `assessments` tables) | |
| 1.6.1b | §6 A versioned policy reads both and decides in a fixed order | met | `evidence.rs:1-14,146-243`; `digest.rs:6` `POLICY_VERSION = "evidence-policy/v2"` is part of every currency key; `evidence_props.rs::order_does_not_matter`, `::current_independent_failure_always_wins` (pass) | |
| 1.6.1c | §6 Strength scale: observed > tested > static; blocked and failed are never a pass | met | `records.rs:53-75`; `evidence_props.rs::claims_never_pass_independent_criteria` | |
| 1.6.1d | §6 Each criterion names its minimum strength and producer (self or independent) | met | `records.rs:196-207`; `evidence.rs:114,227`; `checks.rs::the_run_must_come_from_an_allowed_producer_and_this_tree` | |
| 1.6.1e | (Addition) interlock runs checks itself, baselines on the input snapshot, and runs that tested nothing never count | met | `evidence.rs:125-144,192-216`; `crates/interlock-core/src/vacuity.rs`; `checks.rs::a_run_that_checked_nothing_never_counts` (pass), `run_copilot.rs::a_worker_that_rewrites_the_check_is_rejected_and_the_next_must_really_fix_it` (pass) | Goes beyond the design. Documented in README "Changes" (check runs as a tenth record). |

### 1.7 Lifecycle guards and transitions (diagram plus guard list, design lines 594-606)

| # | Signal (guard / enforced by / fault test) | Status | Evidence | Notes |
|---|---|---|---|---|
| 1.7.1 | **G1** pending to ready. Guard: dependencies done; snapshot of repository, base commit and **a hash of untracked inputs**. Enforced: `ready()` cannot be called without a snapshot. Fault test: an unfinished dependency stays pending | met | `lifecycle.rs:205-232` (takes `snapshot: Snapshot`, not an Option); `store/src/lib.rs:544`; `lifecycle.rs::g1_waits_for_dependencies` (pass, 2.1) | Weakness: no untracked-input hash is ever computed. `run.rs:320` always sets `untracked_hash: None`; it can only be passed by hand (`task ready --untracked-hash`, `main.rs:874`). |
| 1.7.2 | **G2** ready to running. Guard: new attempt at epoch n+1; **the effective grant covers the workflow's required tools**; every required capability or a declared fallback. Enforced: one transaction. Fault test: a missing capability with no fallback goes to blocked | partial | `lifecycle.rs:244-309`; `store/src/lib.rs:560-620` (one transaction); `lifecycle.rs::g2_blocks_when_a_required_capability_has_no_fallback` (pass), `::g2_uses_declared_fallbacks_for_optional_capabilities` | `check_start` compares action **classes** only (`lifecycle.rs:254`). A grant whose deny list removes a tool the workflow needs (for example `shell`) still passes G2. |
| 1.7.3 | **G3** running to awaiting verification. Guard: the current attempt id and epoch, plus an output tree. Enforced: old-epoch results stored as superseded. Fault test: after cancel and respawn, the late result is rejected | met | `lifecycle.rs:373-417`; `store/src/lib.rs:622-694`; `invariants.rs::invariant_4_a_late_worker_cannot_advance_the_task` (pass) | Also rejects results that leave scope or touch check files (`lifecycle.rs:351`). |
| 1.7.4 | **G4** awaiting verification to verified. Guard: every required criterion has current evidence at or above its minimum, from an allowed producer, and no current independent failure. Enforced: versioned policy over append-only records. Fault test: verifier fails, then the worker reports pass: the task stays failed | met | `lifecycle.rs:491-497`; `evidence.rs:146,279`; `invariants.rs::invariant_2_worker_cannot_override_a_verifier_failure` (pass) | The test asserts R1 (back to ready), never verified, which matches the design's R1 guard. The design's own wording, "stays failed", conflicts with its R1 guard; the build follows R1. |
| 1.7.5 | **G5** verified to integrating. Guard: integration required, landing authority granted, operation id allocated. Enforced: the operation row is written before any forge call. Fault test: without authority the task blocks at verified, with the reason | met | `lifecycle.rs:533-559`; `store/src/lib.rs:1167`; `lifecycle.rs::g5_without_landing_authority_blocks_at_verified` (pass); `deliver.rs::without_landing_authority_the_task_blocks_at_verified_and_the_forge_is_untouched` (pass); `evidence/forge/fault-tests/p4-no-landing-authority/` | |
| 1.7.6 | **G6** integrating to done. Guard: the forge confirms a merge of the expected head, and the operation is reconciled. Enforced: `--match-head-commit`; open operations reconciled on restart. Fault test: a head that moves during the merge is refused, then R2 | met | `lifecycle.rs:575-603`; `crates/interlock-core/src/delivery.rs:220`; `gh.rs:267`; `deliver.rs::a_head_that_moves_during_the_merge_is_refused_and_r2_sends_the_task_back` (pass); `evidence/forge/fault-tests/p4-moved-head/` | Against the fake gh only (1.16.1). |
| 1.7.7 | **G7** verified to done, no integration. Guard: the workflow needs no delivery. Enforced: the workflow declares it. Fault test: an investigation completes without the forge | met | `lifecycle.rs:505-507`; `workflow.rs:121` (`Integration::Never` for investigations); `lifecycle.rs:173` (refuses `integration_required` for those); `guided_copilot.rs::an_investigation_runs_end_to_end_in_guidance_mode` (pass, G1-G4, G7) | |
| 1.7.8 | **R1** awaiting verification to ready. Guard: a current assessment failed and the budget has room; fix brief from records. Enforced: budget checked in the same transaction. Fault test: with the budget spent, the task fails instead | met | `lifecycle.rs:470-490`; `store/src/lib.rs:1075`; `lifecycle.rs::failed_check_sends_work_back_through_r1_then_fails_when_budget_is_spent` (pass); `run_copilot.rs::a_failed_verification_sends_the_work_back_and_the_rework_is_verified` (fix brief carries the note) | |
| 1.7.9 | **R2** verified or integrating to awaiting verification. Guard: input, code, policy or check definition changed, or head or base moved. Enforced: currency compared at read time. Fault test: a rebase after verification invalidates evidence and landing is refused | met | `lifecycle.rs:498-504,607`; `invariants.rs::invariant_3_a_rebase_after_verification_sends_the_task_back` (pass); `deliver.rs::a_changed_base_invalidates_the_evidence_and_the_rebased_tree_lands_after_reverification` (pass); `forge_cli.rs::r6_a_tree_recorded_while_integrating_is_r2_under_interlock_run` | "Input changed" covers the base commit only. Untracked inputs are never hashed (1.7.1). |
| 1.7.10 | **R3** (added) running to ready on cancel or timeout | deviates | `lifecycle.rs:515-529`; `run_robustness::a_session_timeout_retries_until_the_attempts_are_spent` (pass) | Documented in README "Changes". Defensible: the design's cancel-and-respawn fault test needs this path. |
| 1.7.11 | **blocked** from any active state. Guard: a capability is missing, no independent verifier, a policy denial, or an operator gate. Enforced: records where it stopped and resumes there. Fault test: unblocking returns to the exact state | met | `lifecycle.rs:629-647` (`resume_point`); `lifecycle.rs::g2_blocks_…` asserts unblock returns to Ready (pass); `::a_missing_independent_verifier_blocks_and_keeps_the_work` (pass) | A policy denial blocks the task only for interlock's own actions (no landing authority, a revoked grant: `operations.rs:188-196`). An agent's denied tool call is refused and the session goes on. |
| 1.7.12 | **failed** from any active state. Guard: budget exhausted or an unrecoverable error. Enforced: a synthetic failure report is always written. Fault test: an attempt that dies without reporting still yields a failure report | met | `store/src/lib.rs:774` `reconcile_attempt` (synthetic end); `run.rs:403`; `sessions.rs::reconcile_attempt_ends_a_running_attempt_with_a_synthetic_report` (pass); `run_robustness::a_session_that_died_with_its_supervisor_is_reconciled_and_retried` (pass) | |
| 1.7.13 | **cancelled** from any active state. Guard: the operator cancels. Enforced: running attempts cancelled through the adapter and reconciled to terminal rows. Fault test: after cancel, no attempt runs | met | `store/src/lib.rs:1148` `stop`; `invariants.rs::cancel_leaves_no_attempt_running` (pass); `run_robustness::task_cancel_from_another_terminal_stops_the_running_session` (pass); `run_fake_host::task_cancel_stops_a_session_whose_supervisor_is_gone` (pass) | |

### 1.8 Invariants (§7)

| # | Invariant (mechanism / fault test) | Status | Evidence | Notes |
|---|---|---|---|---|
| 1.8.1 | 1. No current evidence, no pass. G4 reads only current evidence; a criterion with only stale evidence stays awaiting | met | `evidence.rs:164-169`; `invariants.rs::invariant_1_no_current_evidence_no_pass` (pass) | |
| 1.8.2 | 2. Workers cannot override verifiers or grant themselves authority. Separate tables, role from the attempt, grants only via the operator path. Fault test: verifier fails, worker then reports pass: still failed | partial | Override half: `invariants.rs::invariant_2_worker_cannot_override_a_verifier_failure`, `::invariant_2_records_cannot_be_rewritten_in_place` (pass); literal self-grant denied: `run_copilot.rs::the_stop_guard_holds_the_worker_and_hooks_deny_what_it_was_not_granted` (pass) | **Probe:** the headless hook (`INTERLOCK_ATTEMPT=… interlock hook pre-tool-use`) denies `interlock grant create …` (exit 2) but **allows** `I=interlock; $I grant create …` (exit 0, no output). `interlock policy check` classifies that command, and `python3 -c "subprocess.run(['interlock','grant','create'])"`, as `local_reversible allow`. The CLI does not refuse `grant create` inside an attempt (1.3.13), and a landing grant would let `interlock run` merge. README "What is not proven" admits hooks read command text, but the CLI-level check the forge commands get is missing here. |
| 1.8.3 | 3. Changes invalidate evidence. Currency compared at read time, nothing deleted. Fault test: a rebase after verification sends the task back through R2 | met | `invariants.rs::invariant_3_a_rebase_after_verification_sends_the_task_back` (pass) | |
| 1.8.4 | 4. Late workers cannot advance tasks. Results must match the current attempt and epoch. Fault test: cancel, respawn, the old result is stored as superseded | met | `invariants.rs::invariant_4_a_late_worker_cannot_advance_the_task` (pass); `sessions.rs::reconcile_attempt_ends_a_running_attempt_with_a_synthetic_report` | |
| 1.8.5 | 5. Apply and acknowledge together. Unique event ids, one transaction. Fault test: a duplicate is a no-op; kill during apply, then replay | met | `invariants.rs::invariant_5_a_duplicate_event_is_a_no_op`, `::invariant_5_a_failure_mid_apply_leaves_nothing_then_replay_applies_once` (pass); `cli.rs::a_crash_mid_apply_loses_nothing_and_the_replay_applies_once` (pass; `INTERLOCK_FAULT=crash_before_commit` calls `std::process::abort()`, `store/src/lib.rs:459`) | |
| 1.8.6 | 6. Permissions are bounded. Effective grant = user grant ∩ **host policy** ∩ task needs, passed to the adapter. Fault test: a denied tool is unavailable inside the worker session | partial | Fault test: `run_copilot.rs::the_stop_guard_holds_the_worker_and_hooks_deny_what_it_was_not_granted` (pass, offline Copilot: `denials >= 2`, no grant created); the intersection itself: `grants.rs:81-125`, unit test `grants::tests::host_deny_and_grant_deny_both_apply` | The host-policy term is hard-coded: `HostPolicy::open()` at `main.rs:835` and `run.rs:572`, with no configuration that could narrow it. |
| 1.8.7 | 7. External effects are reconciled, never assumed. Operation row before the call; reconcile on start. Fault test: crash right after a merge call; restart finds the merged SHA | met | `forge_cli.rs::invariant_7_a_crash_right_after_the_merge_call_is_reconciled_to_done` (pass); `operations.rs::a_started_operation_survives_a_crash_for_reconcile_to_find` (pass); `deliver_copilot.rs::a_run_killed_right_after_the_merge_call_is_finished_by_the_next_run`; `evidence/forge/fault-tests/invariant-7-*` | Fake gh only. |
| 1.8.8 | §7 Attempt tokens are bookkeeping, not security; tokens identify the caller | met | `store/src/lib.rs:391` `authenticate` against `token_hash`; `invariants.rs::invariant_2_tokens_and_grants` (a forged token is rejected) | |

### 1.9 Authorization (§8)

| # | Design item | Status | Evidence | Notes |
|---|---|---|---|---|
| 1.9.1 | Five action classes | met | `records.rs:116-122`; `crates/interlock-core/src/classify.rs`; tests `classify::tests::classes`, `::force_push_is_irreversible_wherever_the_flag_sits` | |
| 1.9.2 | Conservative profile by default, permissive opt-in | met | `grants.rs:15-29`; `main.rs:670` `--profile`; `cli.rs::policy_check_pauses_irreversible_actions_and_respects_grants` | |
| 1.9.3 | Read: allowed in both profiles | met | `grants.rs:25-26` | |
| 1.9.4 | Local reversible: allowed within task scope (conservative); allowed (permissive) | deviates | Scope is enforced whatever the profile: hook `crates/interlock-supervisor/src/hook.rs:113-133`, G3 `lifecycle.rs:351` | Stricter than the design. Undocumented, but defensible. |
| 1.9.5 | External reversible: ask or a grant (conservative); allowed (permissive) | met | `grants.rs:157-167`; `hook.rs:137-140` (headless turns ask into deny); `cli.rs::policy_check_…` (`git push origin interlock/fix` asks) | |
| 1.9.6 | Landing needs landing authority in both profiles | met | `classify.rs` (merge commands are `Landing`); workflow role needs exclude landing (`workflow.rs:95,105`), so agents never land; only the forge path lands, under authority | |
| 1.9.7 | Irreversible always pauses; no grant covers it | met | `grants.rs:61-63,95,158-159`; `grants::tests::irreversible_is_never_grantable` (pass); `cli.rs::grants_reject_the_irreversible_class`; `conformance.rs::no_grant_can_cover_the_irreversible_class` | |
| 1.9.8 | Grants replace phrases: scoped to named tasks, up to a class, expiring | met | `grants.rs:75-79`; `grants::tests::grants_expire_and_are_scoped` | |
| 1.9.9 | Landing authority field: none, coordinator, owner, operator | met | `records.rs:126-131`; `crates/interlock-core/src/delivery.rs` `authorize`; `delivery.rs::with_operator_authority_interlock_opens_the_pull_request_but_never_merges`; `regressions.rs::with_operator_authority_interlock_opens_the_pull_request_and_the_operator_merges` | `coordinator` and `owner` behave the same; docs/forge.md documents this. |
| 1.9.10 | On Copilot the effective grant becomes tool flags; deny beats allow | met | `copilot.rs:72-110`, `:183-205`; `adapter/src/lib.rs` test `one_policy_translates_to_each_host`; `cli.rs::one_grant_translates_to_both_hosts` | `--available-tools` is never used, and Copilot read tools are not gated. |
| 1.9.11 | The core separately checks every action it performs, such as forge operations | met | `operations.rs:145-205` re-checks evidence, the tree and authority in the `started` transaction; `deliver.rs::revoking_landing_authority_stops_the_merge_before_the_call`; `operations.rs::a_revoked_grant_fails_the_operation_before_its_call_and_blocks_the_task` | |
| 1.9.12 | Effective grant = user ∩ host policy ∩ task needs | partial | `grants.rs:81` | Host policy is always `HostPolicy::open()` (see 1.8.6). |

### 1.10 Copilot adapter and capability contract (§9)

| # | Design item | Status | Evidence | Notes |
|---|---|---|---|---|
| 1.10.1 | The adapter advertises versioned capabilities; the core decides what to do when one is missing | met | `crates/interlock-core/src/capability.rs:8,89`; `adapter/src/lib.rs:60-69` (`contract_version`); `lifecycle.rs::g2_*` tests | |
| 1.10.2 | Start, status, collect: SDK sessions over JSON-RPC (verify in S2) | deviates | `copilot.rs:72` (`-p`, `--output-format json`); `copilot.rs:139-180` parses JSONL | **Undocumented** as a departure from the SDK. Works offline (`run_copilot.rs`). |
| 1.10.3 | Cancel: SDK session cancel | deviates | `adapter/src/session.rs:512` `contain` (process-group kill plus marker sweep) | README's host table says "kill the process group" but does not call it a change from the design. The tests pass (`run_robustness::sigint_*`, `::task_cancel_*`). |
| 1.10.4 | Native delegation: custom agents as isolated sub-agents with lifecycle events | deviates | docs/skills.md:104 ("Copilot gets no custom agent"); `interlock verify` launches the verifier instead (`run.rs:592`) | Documented and defensible: Copilot's hooks cannot name the calling subagent. |
| 1.10.5 | Parallel work (`--fleet`), optional | met-untested | `copilot.rs:57` reports `Parallel` from process separation; the supervisor runs sessions one after another (`run.rs:296-392`) | Nothing runs in parallel. Fine, since the design marks this optional. |
| 1.10.6 | Tool restriction: `--available-tools`, `--allow-tool`, `--deny-tool`; deny wins | met | `copilot.rs:46-50,87-88`; `run_copilot.rs::the_stop_guard_…` (pass) | `--available-tools` is required for detection but never passed. |
| 1.10.7 | Model selection: `--model` | met-untested | `copilot.rs:93-95`; Claude Code `claude_code.rs:79-80` | The Copilot plan test (`copilot.rs` `plans_a_session`) sets a model but never asserts `--model` in the arguments. Model choice is shown live only on Claude Code (the eval manifests record `claude-sonnet-5-5`). |
| 1.10.8 | Headless one-shot: `copilot -p` with `--agent` | partial | `-p` yes (`copilot.rs:80`); `--agent` is never passed | |
| 1.10.9 | Workspace isolation: one git worktree per attempt, made by interlock | met | `run.rs:1095` `fresh_worktree`; guided `--worktree auto`; `run_copilot.rs::a_bug_fix_runs_to_done_on_copilot` asserts the user's `calc.py` is untouched | |
| 1.10.10 | Session resume not used; a fresh brief from records instead | met | A fresh `--session-id` per session (`run.rs:690`); brief from records (`brief.rs`) | |
| 1.10.11 | Missing parallelism: steps run one after another | met-untested | Always sequential (`run.rs` loop) | |
| 1.10.12 | Missing model selection: the session's current model is used **and recorded** | partial | `workflow.rs:87` declares the `CurrentModel` fallback | The model in use is never read from the host or recorded (1.5.4). |
| 1.10.13 | No independent verifier: the task waits with its work kept, and a second pass by the same agent never counts | met | `lifecycle.rs:313-331`; `lifecycle.rs::a_missing_independent_verifier_blocks_and_keeps_the_work` (pass); `guided_cli.rs::the_worker_cannot_verify_its_own_work_and_only_a_bound_verifier_counts` (pass) | The task moves to `blocked` with `resume_point = awaiting_verification`; it does not stay in `awaiting_verification`. Same effect. |
| 1.10.14 | No enforced tool restriction where required: the step is refused; advisory scope never substitutes | met | `workflow.rs:80-82` (`ToolRestriction` or `PerCallPolicy`, no fallback); the generic blocked test `lifecycle.rs::g2_blocks_…`; recorded run `evidence/runtime/flake/restart-test-blocked-run.txt` ("host is missing: tool restriction") | |
| 1.10.15 | Pin the Copilot version | met | `crates/interlock-supervisor/src/config.rs:28` `[pins]`; `run_robustness::a_host_version_that_differs_from_its_pin_blocks_the_run` | Pinning is opt-in through `.interlock/config.toml`; nothing is pinned by default. |

### 1.11 Skills and generator (§10)

| # | Design item | Status | Evidence | Notes |
|---|---|---|---|---|
| 1.11.1 | Canonical format: `SKILL.md` (host-neutral), `skill.toml` (`invocation`, `requires`, `pack`), `references/` | met | `skills/*/`; loader checks in `skillgen/src/lib.rs:240-305`; `generate.rs::canonical_sources_are_checked` | |
| 1.11.2 | Model-invoked: `.github/skills/<name>/SKILL.md` with name and description | deviates | `skillgen/src/lib.rs:342-344` emits `interlock-<name>`; `generate.rs::copilot_gets_prefixed_github_skills_…` | Documented in docs/skills.md:33. Defensible: avoids clashing with repository skills of the same name. |
| 1.11.3 | User-invoked: decided by S1 | met | Flag kept (`skillgen/src/lib.rs:536`); S1 evidence `evidence/skills/s1/` rows 1-10 | |
| 1.11.4 | Routed only: reference files under the router, not standalone skills | met | `skills/*/skill.toml` `routers = [...]`; `generate.rs::the_v1_set_and_its_intents` | |
| 1.11.5 | Generator enforces lowercase hyphenated names matching their folder | met | `skillgen/src/lib.rs:118` `valid_name`; `generate.rs::names_are_lowercase_hyphenated_and_match_their_folder` (in the 9/9 run) | |
| 1.11.6 | v1 skill set: route, investigate, design, implement, verify, review | met | `generate.rs::the_v1_set_and_its_intents` (pass) | |
| 1.11.7 | v1 workflows: investigation, bug fix, feature, refactor | met | `workflow.rs:112-147`; all four ran headless on Claude Code in the evaluation (`eval/taskset/tasks/*/task.toml` `workflow =`; `evidence/evaluation/claude-code-v3/report.md`) | Guided end-to-end runs exist only for investigation and bug fix (docs/skills.md:208). |
| 1.11.8 | Principle skills become routed references; the machine-checkable subset lives in the core | met | Routed: `skills/{prove-it-works,fix-root-causes,…}/skill.toml`; core: `evidence.rs:125` (baselines), `vacuity.rs`, `lifecycle.rs:351` (scope) | |
| 1.11.9 | Plain Agent Skills output: standard SKILL.md, intent kept in metadata | met | `generate.rs::plain_agent_skills_keep_the_intent_in_metadata_and_validate_on_disk` (pass) | The validator is interlock's own (`skillgen/src/validate.rs`); no third-party validator was run. |

### 1.12 Plan and gates (§12)

| # | Design item | Status | Evidence | Notes |
|---|---|---|---|---|
| 1.12.1 | **S1** skill load test on pinned Copilot | met | `evidence/skills/README.md` rows 1-10; `evidence/skills/s1/*` | Copilot 1.0.91 offline. Row 4 drove the real TUI. |
| 1.12.2 | **S2** Rust SDK parity: sessions, custom agents as sub-agents with lifecycle events, tool permissions, cancel, model selection | not met | No code, test, record or document mentions S2 or the SDK (grep in 1.1.4) | Undocumented. |
| 1.12.3 | **S3** eval baseline: a real repo, 10-15 closed issues with known fixes, Copilot's ordinary workflow run now | partial | Stand-in: 11 synthetic tasks (`eval/taskset/`, `frozen.lock.json`); the `plain` condition on Claude Code (`evidence/evaluation/claude-code-v3/`) | Documented as a stand-in (docs/evaluation.md:5, README "What is not proven"). Not a real repository, not closed issues, not Copilot. |
| 1.12.4 | **S4** JSON Schema for every §5 record | met | `schemas/` (task, criterion, attempt, result, claim, assessment, event, grant, operation, plus check_run and common); `conformance.rs` | |
| 1.12.5 | P0 gate: emission table settled | met | docs/skills.md:96-104, S1 rows | |
| 1.12.6 | P0 gate: adapter language chosen (Rust or Node sidecar) | not met | See 1.1.4, 1.4.9, 1.12.2 | The build chose a third option, a Rust CLI subprocess, without S2 and without recording the decision. |
| 1.12.7 | P0 gate: baseline recorded | partial | See 1.12.3 | |
| 1.12.8 | P0 gate: schemas v0 reviewed | not verifiable here | No evidence README or review record covers the schemas. The README's "reviewed by an independent agent" refers to workstreams, and no schemas review is filed | |
| 1.12.9 | P1 scope: canonical v1 skills, generator, capability inspection, **schemas v1** | partial | Skills, generator and inspection are present | Schemas are still `/v0/` (1.5.1). |
| 1.12.10 | P1 gate: investigation and bug fix end to end on pinned Copilot in guidance mode | partial | `guided_copilot.rs` (both tests; investigation re-run here: pass); `evidence/skills/p1/copilot-*`; `evidence/skills/review-fixes/guided-copilot-3-runs.txt` | On the real Copilot CLI, but a scripted model chose every tool call. With a real model the gate is shown only on Claude Code (`evidence/skills/p1/claude-code-*`, `evidence/skills/integration/*`). |
| 1.12.11 | P1 gate: every generated skill loads | met | `skills_load.rs::every_generated_skill_loads_on_copilot` (pass), `::every_model_invoked_skill_reaches_copilots_model_through_the_skill_tool` (pass), `::a_person_can_type_the_design_skill_in_copilots_interactive_session`; `evidence/skills/load/` | |
| 1.12.12 | P1 gate: the plain Agent Skills output passes static validation | met | `generate.rs::every_target_validates`, `::plain_agent_skills_…` (pass); `evidence/skills/validate/` | |
| 1.12.13 | P2 scope: Rust CLI, SQLite store, guards, claims and assessments, event ingestion with ack, briefs | met | Crates as in 1.4 | |
| 1.12.14 | P2 gate: completion without evidence rejected | met | `invariants.rs::invariant_1_no_current_evidence_no_pass` (pass); no command sets a state directly (`interlock task --help` lists none) | |
| 1.12.15 | P2 gate: a worker pass cannot override a verifier failure | met | `invariants.rs::invariant_2_worker_cannot_override_a_verifier_failure` (pass) | |
| 1.12.16 | P2 gate: duplicate events are no-ops | met | `invariants.rs::invariant_5_a_duplicate_event_is_a_no_op` | |
| 1.12.17 | P2 gate: stale results are superseded | met | `invariants.rs::invariant_4_…` (pass) | |
| 1.12.18 | P2 gate: stale evidence re-verifies | met | `invariants.rs::invariant_3_…` (pass) | |
| 1.12.19 | P2 gate: killed mid-write and restarted, actionable work survives | met | `cli.rs::a_crash_mid_apply_loses_nothing_and_the_replay_applies_once` (pass) | |
| 1.12.20 | P2 gate: capabilities masked in the adapter produce the declared fallback or blocked | partial | Pure-core tests only: `lifecycle.rs::g2_blocks_…`, `::g2_uses_declared_fallbacks_…`, `::a_missing_independent_verifier_…` | No test masks a capability in an adapter's `HostReport` and runs it through the store, CLI or supervisor. The only end-to-end instance is an accidental one: the recorded flake (`evidence/runtime/flake/restart-test-blocked-run.txt`). |
| 1.12.21 | P3 scope: Copilot adapter for both paths, supervisor, budgets, timeouts, cancellation, grants as tool restrictions | met | `run.rs`, `copilot.rs`, `export.rs`, `config.rs`; run_robustness and run_copilot suites | The adapter is not on the SDK (1.1.4). |
| 1.12.22 | P3 gate: termination has a tested outcome | met | `run_robustness::sigint_cancels_the_session_and_the_exported_work_resumes` (pass), `::sigterm_stops_a_run_the_same_way`; `run_fake_host::sighup_stops_a_run_like_sigterm` | |
| 1.12.23 | P3 gate: timeout | met | `run_robustness::a_session_timeout_retries_until_the_attempts_are_spent` (pass) | |
| 1.12.24 | P3 gate: late completion | met | `sessions.rs::reconcile_attempt_ends_a_running_attempt_with_a_synthetic_report` (pass); `invariants.rs::invariant_4_…` | |
| 1.12.25 | P3 gate: policy denial | met | `run_copilot.rs::the_stop_guard_holds_the_worker_and_hooks_deny_what_it_was_not_granted` (pass) | |
| 1.12.26 | P3 gate: supervisor restart | met | `run_robustness::a_supervisor_killed_mid_session_reattaches_and_carries_on` (pass), `::a_session_that_died_with_its_supervisor_is_reconciled_and_retried` (pass); live Claude Code `evidence/runtime/live-claude-reattach-integrated/` (checked in 2.2) | |
| 1.12.27 | P3 gate: every started attempt reconciles to a terminal row | met | `run_robustness.rs:198-210` `assert_every_attempt_ended`, called by the suite | |
| 1.12.28 | P3 gate: a denied tool really is unavailable to the worker | met | `run_copilot.rs::the_stop_guard_…` (offline Copilot: the real CLI enforces `--deny-tool`) | With a real Copilot model: see 1.16.2. |
| 1.12.29 | P4 scope: forge adapter with pinned merges, readiness observer, operation reconciliation | met | `crates/interlock-forge/src/lib.rs:206` `readiness`; `deliver.rs:184` `reconcile`; `interlock-forge/tests/readiness.rs` | Fake gh only. |
| 1.12.30 | P4 gate: a moved head refuses the merge | met | `deliver.rs::a_head_that_moves_…` (pass); `forge_cli.rs::p4_a_moved_head_refuses_the_merge`; `evidence/forge/fault-tests/p4-moved-head/` | The refusal comes from the fake gh. GitHub's real refusal message is not verifiable here (1.16.1). |
| 1.12.31 | P4 gate: a changed base invalidates evidence | met | `deliver.rs::a_changed_base_…` (pass); `forge_cli.rs::p4_a_changed_base_invalidates_the_evidence`; `regressions.rs::the_base_is_read_with_git_right_before_the_merge` | |
| 1.12.32 | P4 gate: the evaluation report compares against the S3 baseline with repeated runs | not met | `evidence/evaluation/claude-code-v3/report.md`; `evidence/evaluation/claude-code-v4/report.md`; `evidence/evaluation/checks/compare_v3_v4.out.json` | There is no S3 baseline (1.12.3); every run is on stand-in tasks, on Claude Code. v4 added repeats (below) and compares itself with v3, but its harder-task runs are in-sample: the prompt changes were written against those tasks' failures, and the host moved from 2.1.289 to 2.1.291 between the runs (docs/evaluation.md, "Results v4"). |

### 1.13 Evaluation (§13)

| # | Design item | Status | Evidence | Notes |
|---|---|---|---|---|
| 1.13.1 | Three conditions on the same frozen task set: ordinary workflow, skills alone, skills with interlock | met | `eval/harness.py`; `evidence/evaluation/claude-code-v3/report.md` (15 runs per condition) | On stand-in tasks. |
| 1.13.2 | Copilot version and model pinned | deviates | Claude Code 2.1.289, `claude-sonnet-5-5`, effort medium (`claude-code-v3/manifest.json`); Copilot ran only as scripted-model plumbing (`evidence/evaluation/copilot-plumbing/`) | Documented (README "What is not proven", docs/evaluation.md:282). |
| 1.13.3 | Include small fixes, cross-module features, refactors, investigations, forced interruptions | met | docs/evaluation.md:58-70; `eval/interrupt.py`; recovery 3/3 per condition | |
| 1.13.4 | Repeat each run | partial | v3: 4 tasks × 2 repeats and 7 tasks × 1; v4: a second repeat of all three conditions on 6 of the 7 original tasks (the budget stopped it before `py-split-remainder`), and two more repeats of skills and interlock on the 4 harder tasks; v1: 7 tasks × 2 repeats, but only 2 conditions | `py-split-remainder` still has one run per condition. v4's repeats of skills and interlock on the harder tasks ran changed prompts and skills on a newer host, so they are new measurements, not repeats of v3's, and they are in-sample; `plain` was not rerun there. |
| 1.13.5 | Measure accepted outcomes, missed defects, unsupported completion claims, scope violations, recovery success, human interventions, wall time, tokens and cost | met | `claude-code-v3/report.md` rows for every metric, including output tokens | Human interventions are "n/a (headless)": the guided path was not part of the evaluation. |
| 1.13.6 | Record any unobservable metric as unavailable | met | report.md "Runs with complete cost 12/15"; docs/evaluation.md:178 (Copilot cost unavailable) | |
| 1.13.7 | Judge anonymized outputs; prefer executable checks | met | `eval/harness.py:1300-1317` `judge`; hidden `judge.py` per task; `python3 eval/test_harness.py`: 36 pass (re-run, 2.1) | |
| 1.13.8 | Agents' confidence is not a metric | met | docs/evaluation.md:123 | |
| 1.13.9 | The first goal is falsifiable: better reliability with understood overhead | met | README:107-111; docs/evaluation.md:261-277 | Reported honestly. The data does not support the hypothesis: interlock 12/15 accepted against plain 14/15 and skills 15/15, at 2.5× cost, and the verifier passed all 3 failures. |

### 1.14 Walkthrough (§11)

| # | Design item | Status | Evidence | Notes |
|---|---|---|---|---|
| 1.14.1 | Export-retry bug fix on Copilot with native delegation: route skill, task create with two criteria, G1/G2, a custom-agent worker, G3, a custom-agent verifier limited to read and test tools, G4, then G5 and the pinned merge and G6 if authorized; fault paths in the same run (cancel/respawn superseded; rebase then R2) | partial | `examples/export-retry/`; live Claude Code G1-G6 to the fake gh: `evidence/forge/live-claude-code/` (row 6); offline Copilot: `deliver_copilot.rs::walkthrough_step_7_a_verified_bug_fix_lands_through_g5_and_g6_on_copilot` (pass) | Not on Copilot with a model; no native delegation or custom agents; GitHub is faked. The fault paths are tested separately (1.8.3, 1.8.4), not in the same run. The README's "recorded runs" of October 5 exist only as prose (discrepancy 3.10). |

### 1.15 Risks and open questions (§14)

| # | Design item | Status | Evidence | Notes |
|---|---|---|---|---|
| 1.15.1 | Risk: skill loading on Copilot could block phase 1; S1 settles it | met | S1 evidence (1.12.1) | |
| 1.15.2 | Risk: the Rust SDK is new; S2 decides Rust-native or Node sidecar | not met | (1.12.2) | Not addressed, not mentioned. |
| 1.15.3 | Risk: one host proves the runtime, not portability; host-neutral schemas and validated Agent Skills output keep a second host cheap | met | Two hosts behind one trait (`adapter/src/lib.rs:159`); `generate.rs::every_target_validates` | A second host was actually built. |
| 1.15.4 | Risk: single controller, SQLite on local disk | met | `lock.rs:21`; `run_fake_host::only_one_supervisor_gets_the_lock_however_many_race` | No network-filesystem check (1.3.5). |
| 1.15.5 | Q: keep the name interlock? | met | README:167 states the default | |
| 1.15.6 | Q: default authorization, conservative or permissive? | met | README:168; `grants.rs:16` | |
| 1.15.7 | Q: which repository for the S3 baseline? | partial | README:170; docs/evaluation.md:188-207 (swap-in procedure) | Still open; a stand-in is used. |
| 1.15.8 | Q: port pstack's PR watcher to Rust, or a TypeScript sidecar? | partial | Rust readiness polling in `interlock-forge` (`lib.rs:206`, `deliver.rs`) | Answered implicitly. README's §14 section (lines 163-170) does not list it. |
| 1.15.9 | Q: do six skills and four workflows cover what is needed first? | not verifiable here | none | The operator has to answer this; README's §14 list omits it. |
| 1.15.10 | Q: is GitHub the only forge for v1? | met | README:169; docs/forge.md:218 | |

### 1.16 Validation context (what the design's "validated on" needs and this container cannot give)

| # | Item | Status | Evidence | Notes |
|---|---|---|---|---|
| 1.16.1 | Delivery against live GitHub: real `--match-head-commit` refusal text, `headRefOid` lag, auto-merge and merge queues, branch protection, credentials | not verifiable here | `evidence/forge/gh-auth-status.txt` (GH_TOKEN invalid); docs/forge.md:202-209 | Stated honestly in README "What is not proven". |
| 1.16.2 | Copilot CLI with a real model, both paths: whether a model follows the skills, the guided ask prompt, real tool behaviour | not verifiable here | Every Copilot run uses a scripted model (`crates/interlock-cli/tests/fake_model`, `guided_model`) | Stated in README "What is not proven". |

---

## 2. Spot checks

### 2.1 Tests run

Run one at a time with `CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 INTERLOCK_REQUIRE_HOSTS=1 INTERLOCK_COPILOT_BIN=<scratchpad>/cop/node_modules/.bin/copilot cargo test -q -p <crate> --test <file> <name> -- --exact`. Because `INTERLOCK_REQUIRE_HOSTS=1` was set, no Copilot test could pass by skipping. Script: `scratchpad/audit/spot.sh`, `spot2.sh`; results: `scratchpad/audit/results.txt`; logs: `scratchpad/audit/log-*.txt`.

| # | Test | Result |
|---|---|---|
| 1 | `interlock-core --test lifecycle g1_waits_for_dependencies` | pass |
| 2 | `interlock-core --test lifecycle g2_blocks_when_a_required_capability_has_no_fallback` | pass |
| 3 | `interlock-core --test lifecycle failed_check_sends_work_back_through_r1_then_fails_when_budget_is_spent` | pass |
| 4 | `interlock-core --test lifecycle g5_without_landing_authority_blocks_at_verified` | pass |
| 5 | `interlock-core --test lifecycle a_missing_independent_verifier_blocks_and_keeps_the_work` | pass |
| 6 | `interlock-core --test evidence_props current_independent_failure_always_wins` (proptest) | pass |
| 7 | `interlock-core --test evidence_props stale_evidence_never_counts` (proptest) | pass |
| 8 | `interlock-core --test checks a_run_that_checked_nothing_never_counts` | pass |
| 9 | `interlock-core --lib grants::tests::irreversible_is_never_grantable` | pass |
| 10 | `interlock-store --test invariants invariant_1_no_current_evidence_no_pass` | pass |
| 11 | `interlock-store --test invariants invariant_2_worker_cannot_override_a_verifier_failure` | pass |
| 12 | `interlock-store --test invariants invariant_3_a_rebase_after_verification_sends_the_task_back` | pass |
| 13 | `interlock-store --test invariants invariant_4_a_late_worker_cannot_advance_the_task` | pass |
| 14 | `interlock-store --test invariants invariant_5_a_failure_mid_apply_leaves_nothing_then_replay_applies_once` | pass |
| 15 | `interlock-store --test invariants cancel_leaves_no_attempt_running` | pass |
| 16 | `interlock-store --test operations a_started_operation_survives_a_crash_for_reconcile_to_find` | pass |
| 17 | `interlock-store --test sessions reconcile_attempt_ends_a_running_attempt_with_a_synthetic_report` | pass |
| 18 | `interlock-schema --test conformance every_record_kind_round_trips_through_its_schema` | **fails to compile** (E0425: cannot find type `Validators`): the test needs the `validate` feature and the crate declares no `required-features` |
| 18b | the same with `--features validate` | pass |
| 19 | `interlock-cli --test cli a_crash_mid_apply_loses_nothing_and_the_replay_applies_once` | pass |
| 20 | `interlock-forge --test deliver a_head_that_moves_during_the_merge_is_refused_and_r2_sends_the_task_back` | pass |
| 21 | `interlock-forge --test deliver a_changed_base_invalidates_the_evidence_and_the_rebased_tree_lands_after_reverification` | pass |
| 22 | `interlock-forge --test deliver every_forge_call_finds_its_operation_already_started` | pass |
| 23 | `interlock-forge --test deliver without_landing_authority_the_task_blocks_at_verified_and_the_forge_is_untouched` | pass |
| 24 | `interlock-cli --test forge_cli invariant_7_a_crash_right_after_the_merge_call_is_reconciled_to_done` | pass |
| 25 | `interlock-cli --test forge_cli r3_forge_commands_are_the_operators_inside_an_attempt` | pass |
| 26 | `interlock-skillgen --test generate` (all 9) | pass |
| 27 | `interlock-cli --test run_fake_host task_cancel_stops_a_session_whose_supervisor_is_gone` | pass |
| 28 | `interlock-cli --test run_fake_host an_escaped_worker_process_cannot_forge_the_verifiers_evidence` | pass |
| 29 | `interlock-cli --test run_copilot the_stop_guard_holds_the_worker_and_hooks_deny_what_it_was_not_granted` (offline Copilot) | pass |
| 30 | `interlock-cli --test run_copilot a_worker_that_rewrites_the_check_is_rejected_and_the_next_must_really_fix_it` (offline Copilot) | pass |
| 31 | `interlock-cli --test run_robustness a_session_that_died_with_its_supervisor_is_reconciled_and_retried` (offline Copilot) | pass |
| 32 | `interlock-cli --test run_robustness a_session_timeout_retries_until_the_attempts_are_spent` | pass |
| 33 | `interlock-cli --test run_robustness a_supervisor_killed_mid_session_reattaches_and_carries_on` | pass |
| 34 | `interlock-cli --test run_robustness sigint_cancels_the_session_and_the_exported_work_resumes` | pass |
| 35 | `interlock-cli --test run_robustness task_cancel_from_another_terminal_stops_the_running_session` | pass |
| 36 | `interlock-cli --test skills_load every_generated_skill_loads_on_copilot` | pass |
| 37 | `interlock-cli --test skills_load every_model_invoked_skill_reaches_copilots_model_through_the_skill_tool` | pass |
| 38 | `interlock-cli --test guided_copilot an_investigation_runs_end_to_end_in_guidance_mode` | pass |
| 39 | `interlock-cli --test guided_cli the_worker_cannot_verify_its_own_work_and_only_a_bound_verifier_counts` | pass |
| 40 | `interlock-cli --test deliver_copilot walkthrough_step_7_a_verified_bug_fix_lands_through_g5_and_g6_on_copilot` | pass |
| 41 | `interlock-core --test delivery every_call_needs_landing_authority_at_the_time_of_the_call` | pass |
| 42 | `python3 eval/test_harness.py` (harness unit tests) | 36 of 36 pass |

Totals: 42 Rust test invocations plus the harness suite. 41 pass as cited. One (#18) does not compile as cited and passes once its feature is enabled. I skipped `skills_load::every_generated_skill_and_the_verifier_load_on_claude_code` and `deliver_live` to avoid invoking `claude`.

### 2.2 Probes against the built binary (scratch repository `scratchpad/audit/probe-grant`)

| Probe | Result |
|---|---|
| `INTERLOCK_ATTEMPT=att-fake interlock grant create --principal worker --tasks '*' --classes landing --landing coordinator --origin self` | exit 0; the grant is created |
| `INTERLOCK_ATTEMPT=att-fake interlock reconcile` | refused: "`interlock reconcile` is the operator's; it does not run inside an attempt" |
| Headless `interlock hook pre-tool-use` with an attempt, Bash `interlock grant create …` | deny, exit 2 |
| Same hook, Bash `I=interlock; $I grant create …` | allow (exit 0, empty response) |
| `interlock policy check --shell 'python3 -c "import subprocess; subprocess.run([\"interlock\",\"grant\",\"create\"])"'` | `local_reversible`, `allow` |
| `ldd target/debug/interlock` | dynamically linked (libc, libm, libgcc_s) |

### 2.3 Evidence-README claims checked against raw files

| # | Claim (where) | Raw file(s) | Verdict |
|---|---|---|---|
| 1 | Guided Claude Code bug fix: done in 48 s, 15 turns, $0.26, no denials; G1-G4, G7; verifier bound `subagent` (evidence/skills/README.md row 20) | `evidence/skills/p1/claude-code-bug-fix/claude-transcript.jsonl` result event (`num_turns 15`, `total_cost_usd 0.2634`, `duration_ms 48129`, `permission_denials []`); `task-log.json`; `attempts.json` (verifier `binding.via = subagent`, `agent_type interlock:verifier`); `assessments.json` (`bound_via subagent`); `check_runs.json` (repro base exit 1, output exit 0) | holds |
| 2 | Integrated live re-attach: `reattached ["att-8017943935334f7a"]`, `reconciled []`, log G1 G2 G3 G4 G7, $0.132, pin match (evidence/runtime/README.md §7) | `live-claude-reattach-integrated/07-run-2-report.json`, `08-task-log.json`, `00-timeline.txt` (worker $0.0716 plus verifier $0.0609) | holds |
| 3 | Invariant 7 crash then reconcile: one `gh pr merge` call in total, G6 through reconcile (evidence/forge/README.md §3) | `fault-tests/invariant-7-crash-then-reconcile/gh-calls.json` (6 calls, exactly one `pr merge`, with `--match-head-commit`), `2-reconcile.json` (G6), `transitions.json` | holds |
| 4 | "15 mutations. 14 make every covering test fail. One (m08b) does not" (evidence/forge/README.md row 5) | `evidence/forge/mutations/summary.json` | **wrong count**: 16 entries, 15 with `all_failed: true`, and only `m08b-signing-allowed` is not. The table below that row lists 16. |
| 5 | Round-2 `run_fake_host` tests all fail on the round-1 build (evidence/runtime/README.md §2) | `run-fake-host-before-fixes.txt`: `0 passed; 16 failed` | holds |
| 6 | Integration: `run_robustness` 10/10, `run_fake_host` 17/17 (evidence/runtime/README.md §7) | `integration-858879e/run_robustness.txt`, `run_fake_host.txt` | holds |
| 7 | Workspace suite: 308 passed, 0 failed, 1 ignored (evidence/evaluation/README.md) | `checks/cargo_test_hosts.out.txt`: the 41 `test result` lines sum to 308/0/1 | holds |
| 8 | Leak flags on `go-dotted-section.plain.r2.c6a447` and `.skills.r1.708554` are false positives: a Go test in `cmd/kvconf/main_test.go` that opens `../../checks/dotted.conf` (evidence/evaluation/README.md) | `claude-code-v3/runs/go-dotted-section.plain.r2.c6a447/run.json` `leak_scan` (kind `other-run`); `raw/session-1.jsonl.gz` contains `cat >> cmd/kvconf/main_test.go … run([]string{"get", "../../checks/dotted.conf", …` | holds. But report.md labels the same flags "Runs whose transcripts reach hidden material: 1 / 1 / 0" (3.8). |
| 9 | Fisher two-sided p = 0.60 (interlock against plain), 0.22 (against skills); 0.20 and 0.57 on the harder runs (docs/evaluation.md:267) | recomputed from report.md counts | holds (0.598, 0.224, 0.200, 0.569) |
| 10 | Paired ratios: interlock/plain cost 2.53 and wall 2.33; skills/plain 1.11 and 1.10; worker $1.09 and verifier $0.91 (README, docs) | `checks/paired_ratios_v3.out.json` | holds |
| 11 | Copilot lists all six generated `interlock-*` skills, `source: project`, `enabled: true` (evidence/skills/README.md row 13) | `evidence/skills/load/copilot-skill-list-generated.json` | holds |
| 12 | Copilot plumbing: 66 runs (evidence/evaluation/README.md) | `copilot-plumbing/runs/` holds 66 directories; report.md shows 22 per condition | holds |
| 13 | "$7.89 charged against the $8.50 cap" (docs/evaluation.md:259; evidence/evaluation/README.md:104) | `claude-code-v3/report.md:7` says "$7.81 charged against the $8.41 budget" | **inconsistent**. The two can be reconciled (the docs add $0.08 of probes and quote the outer cap), but the documents give different numbers. |

---

## 3. Discrepancies between README/docs claims and code or evidence

Overclaims first.

1. **README.md:5**: "Every phase of its plan, P0 to P4, has code, tests and recorded runs. Where a gate could not be met here, the gap is stated under What is not proven." S2 has no code, test or record. The P0 gate item "adapter language chosen (Rust or Node sidecar)" is unmet and appears nowhere in README or docs. The build's Copilot integration is a CLI subprocess, not the SDK and not a Node sidecar, and that is not recorded as a change from the design. (Overclaim and undocumented deviation.)
2. **README.md:140** (invariant 2) and **README.md:3**: "Workers cannot override verifiers or grant themselves authority." The cited test shows only that the literal command `interlock grant create` is denied. `grant create` has no inside-attempt check (`main.rs:1112`), unlike integrate, reconcile and run (`forge.rs:52`), and the headless hook allows `I=interlock; $I grant create …` (2.2). README "What is not proven" covers the hooks' text-only parsing in general. But docs/forge.md:132 says forge commands refuse inside an attempt "whatever the hooks let through", and that statement has no equivalent for grants. (Overclaim.)
3. **README.md:131**: "every confirmed finding became a regression test that fails before its fix." evidence/skills/README.md: item 14 is covered by "skill text" only, and item 15's root cause "not reproduced". evidence/forge/README.md: m08b's covering test passes with the fix undone (acknowledged there). (Overclaim.)
4. **README.md:144** (invariant 6): "a worker's `git push` is denied inside the live session." The session is the real Copilot CLI driven by a **scripted** model; "live" reads as a live model. (Ambiguous wording.)
5. **README.md "Changes from the design draft"** (lines 149-161) does not list several deviations: the Copilot SDK replaced by `copilot -p`; no `interlock-copilot` crate; no process protocol in `interlock-adapter`; tokio dropped; scope enforced under the permissive profile; G2 checking classes but not tools. The host change (Claude Code made first-class, all live evidence on it) is explained under "Hosts", not in the changes list. docs/skills.md:189 cites "the host decision", which is not in the design. The design's §1 says "GitHub Copilot CLI only".
6. **README.md:119**: "Rust types are checked against them by a conformance test." True in a workspace run, but `cargo test -p interlock-schema --test conformance` fails to compile without `--features validate` (2.1 #18).
7. **README.md:101**: "On the four harder tasks." docs/evaluation.md:218 and :265 say three harder tasks plus `py-date-filter`, which is an interruption task and not designated harder.
8. **evidence/evaluation/claude-code-v3/report.md**: "Runs whose transcripts reach hidden material | 1 | 1 | 0." docs/evaluation.md:255 and the evidence README say no run reached hidden material, and the raw record shows a scanner kind of `other-run`, a false positive (2.3 #8). The report's row label is wrong; the docs are right.
9. **evidence/forge/README.md** row 5: "15 mutations. 14 make every covering test fail." `summary.json` has 16 mutations, 15 of which fail every covering test (2.3 #4). (Undercount.)
10. **README.md:73**: "Recorded runs of the export-retry example on Claude Code: the corrected task went from create to done in two sessions, and the original task … was blocked before any session started." The link goes to `examples/export-retry/README.md`, which holds prose and a diff but no raw records. No directory under `evidence/` holds those October 5 runs. The October 6 delivery run (`evidence/forge/live-claude-code/`) is a different run.
11. **docs/evaluation.md:259** and **evidence/evaluation/README.md:104** ("$7.89 charged against the $8.50 cap") against **report.md:7** ("$7.81 charged against the $8.41 budget"): inconsistent figures.
12. **README "Open questions from the draft (§14)"** (lines 163-170) lists four of the six questions. It omits the PR-watcher question (answered implicitly: Rust, in `interlock-forge`) and "do six skills and four workflows cover what you need".
13. **README "What is not proven"** omits two gaps: 7 of 11 tasks ran once in the only three-condition run, though the design says "repeat each run"; and P1 asked for schemas v1, but every `$id` is still `/v0/`.
14. **README.md:26**: the host table lists "Custom agents: `--agent`" for Copilot. interlock never passes `--agent`, and docs/skills.md:104 says Copilot gets no custom agent. This is accurate as a capability list but suggests the build uses it.

Claims I checked that hold: README evaluation numbers (accepted 14/15, 15/15, 12/15; Fisher p; paired ratios); the runtime and forge live-run summaries; "308 passed"; the guided Claude Code runs; the Copilot skill-load results; the "What is not proven" bullets on live GitHub and on Copilot with a real model.

---

## 4. Summary

### 4.1 Counts by status (165 matrix rows)

| Status | Rows |
|---|---|
| met | 114 |
| met-untested | 4 |
| partial | 25 |
| deviates | 14 |
| not met | 4 |
| not verifiable here | 4 |

The 14 deviations break down as follows:

- **Documented and defensible (6):** 1.1.3, 1.5.6, 1.5.7, 1.7.10, 1.10.4, 1.11.2.
- **Documented, with weaker justification (2):** 1.1.1 (host change) and 1.13.2 (evaluation host).
- **Stated as a fact, never called a change from the design (1):** 1.10.3 (process-group cancel).
- **Undocumented (5):** 1.1.4 and 1.10.2 (no SDK; `copilot -p` instead), 1.4.9 (no `interlock-copilot` crate), 1.4.12 (tokio dropped), 1.9.4 (scope enforced under the permissive profile).

Many `met` rows rest on tests against a fake `gh` or the real Copilot CLI driven by a scripted model. The rows say which.

### 4.2 Five most important gaps

1. **S2 never ran, and the Copilot integration is neither option the design allowed** (1.1.4, 1.4.8, 1.4.9, 1.10.2, 1.12.2, 1.12.6, 1.15.2). Instead of the Rust SDK or a Node sidecar behind a process protocol, the build shells out to `copilot -p`. There is no `interlock-copilot` crate and no process protocol, and none of this is documented.
2. **The design's validation host has no live-model evidence** (1.1.1, 1.3.2, 1.12.10, 1.13.2, 1.16.2). Every Copilot run uses a scripted model. Everything that shows a model following the skills, re-attach with a real model, live delivery of the walkthrough, and the whole §13 evaluation ran on Claude Code.
3. **No S3 baseline, and the evaluation gate is unmet** (1.12.3, 1.12.7, 1.12.32, 1.13.4). The 11 tasks are synthetic stand-ins, the plain condition ran on Claude Code, and 7 of 11 tasks ran once. The run that exists falsifies the design's first goal on these tasks: interlock accepted 12/15 against 14/15 (plain) and 15/15 (skills), at about 2.5× the cost, and its verifier passed all three failures. None of these differences is significant at n=15.
4. **The rule that only the operator writes grants is not enforced** (1.3.13, 1.5.9, 1.8.2). `interlock grant create` runs inside an attempt, and the headless hook allows it when the program name is hidden in a variable. A worker could grant itself landing authority, which `interlock run` would then use to merge. The forge commands already carry the one-line CLI check that would close this.
5. **Record and mechanism gaps that weaken stated guards:**
   - The G1 untracked-input hash is never computed (`run.rs:320`).
   - G2 checks action classes, not tools (`lifecycle.rs:254`).
   - Attempts record the requested model, not the reported one, so the "current model used and recorded" fallback records nothing (1.5.4, 1.10.12).
   - Host policy is hard-coded open (1.8.6, 1.9.12).
   - The brief has no upstream results (1.5.12).
   - Schemas are still v0 (1.5.1, 1.12.9).
   - The "masked capability" P2 gate is tested only in the pure core (1.12.20).

### 4.3 Gates

- **P0: partially met.** S1 is met (emission table settled) and S4 is met (schemas exist). S2 was not done, and the adapter language was not chosen between the design's options; the build took an undocumented third path. The S3 baseline is a stand-in on Claude Code, not Copilot on a real repository. Nothing records a review of schemas v0.
- **P1: partially met.** Every generated skill loads on Copilot, and the plain Agent Skills output passes static validation, both tested. Investigation and bug fix ran end to end in guidance mode on pinned Copilot 1.0.91 only with a scripted model; the real-model runs are on Claude Code. Schemas v1 were not cut.
- **P2: met, with one item partial.** Completion without evidence is rejected, a worker cannot override a verifier, duplicate events are no-ops, stale results are superseded, stale evidence is re-verified, and a crash mid-write followed by replay works. Each has a passing test. Capability masking is tested only in the pure core, not through an adapter.
- **P3: met on Copilot CLI offline.** Termination, timeout, late completion, policy denial, supervisor restart, terminal rows for every attempt, and a denied tool being unavailable each have passing tests on the real Copilot CLI with a scripted model. Re-attach was also shown live on Claude Code. Two caveats: the adapter is not on the SDK, and real-model behaviour on Copilot is not verifiable here.
- **P4: partially met.** A moved head refusing the merge and a changed base invalidating evidence are met, against a fake `gh` and a bare repository (live GitHub not verifiable). Readiness and operation reconciliation are met. The gate item "evaluation compared against the S3 baseline with repeated runs" is not met.
