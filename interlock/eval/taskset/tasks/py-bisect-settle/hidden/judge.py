"""Hidden judge for py-bisect-settle: the ANSWER line must name the culprit
commit recorded when the frozen repository was built."""

import json
import os
import re
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "..", ".."))
import judgelib  # noqa: E402

a = judgelib.args()
facts = json.load(open(a.facts))
culprit = facts["culprit"]
all_shas = [c["sha"] for c in facts["commits"]]

lines = [l for l in judgelib.answer_lines(a.answer) if re.match(r"(?i)\W*answer\W*:", l)]
cases = [judgelib.case("answer_given", bool(lines), "\n".join(lines) or "no ANSWER line in the final message")]
named = []
if lines:
    # The last ANSWER line counts.
    named = re.findall(r"\b[0-9a-f]{7,40}\b", lines[-1].lower())
matches = {sha for h in named for sha in all_shas if sha.startswith(h)}
ok = len(named) == 1 and matches == {culprit}
cases.append(
    judgelib.case(
        "answer_names_culprit",
        ok,
        f"named {named}; resolves to {sorted(matches)}",
    )
)
judgelib.emit(cases)
