#!/bin/sh
# Reproduces the bug: a transient failure on the third write, then a retry.
python3 - <<'PY'
from exporter import Sink, export
sink = Sink(fail_at=3)
export([{"id": 1}, {"id": 2}, {"id": 3}], sink)
ids = [r["id"] for r in sink.rows]
assert ids == [1, 2, 3], f"expected [1, 2, 3], got {ids}"
print("ok: no duplicate rows after a retry")
PY
