import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "..", ".."))
import judgelib  # noqa: E402

a = judgelib.args()
# The hidden tests read examples/trip.csv; use the original so an edited
# example cannot change the expected numbers.
import shutil  # noqa: E402

shutil.copy(os.path.join(a.start, "examples", "trip.csv"), os.path.join(a.tree, "examples", "trip.csv"))
judgelib.emit(judgelib.python_cases(a.tree, a.start, os.path.dirname(__file__), ["hidden_csv.py"]))
