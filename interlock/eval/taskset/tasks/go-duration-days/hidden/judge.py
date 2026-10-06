import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "..", ".."))
import judgelib  # noqa: E402

a = judgelib.args()
judgelib.emit(judgelib.go_cases(a.tree, a.start, os.path.dirname(__file__), ["zz_hidden_test.go"]))
