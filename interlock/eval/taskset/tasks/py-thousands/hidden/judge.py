import os
import shutil
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "..", ".."))
import judgelib  # noqa: E402

a = judgelib.args()
# The hidden tests read the examples; use the originals.
for name in ("big.csv", "trip.csv"):
    shutil.copy(os.path.join(a.start, "examples", name), os.path.join(a.tree, "examples", name))
judgelib.emit(judgelib.python_cases(a.tree, a.start, os.path.dirname(__file__), ["hidden_thousands.py"]))
