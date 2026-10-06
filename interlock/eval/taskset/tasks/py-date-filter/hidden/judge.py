import os
import shutil
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "..", ".."))
import judgelib  # noqa: E402

a = judgelib.args()
shutil.copy(os.path.join(a.start, "examples", "trip.csv"), os.path.join(a.tree, "examples", "trip.csv"))
judgelib.emit(judgelib.python_cases(a.tree, a.start, os.path.dirname(__file__), ["hidden_dates.py"]))
