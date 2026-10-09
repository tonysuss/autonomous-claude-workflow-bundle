Since a recent change, `python3 -m tally settle` sometimes prints a transfer of 0.00. For `examples/trip.csv` it now prints `Dan -> Cal  0.00` between the two real transfers; it did not do that before. `checks/settle-repro.sh` reproduces it: the script fails on the current code.

Find the commit that introduced this regression. This is an investigation: do not fix the bug and leave every file as it is. In your final message, say briefly how you confirmed the answer, and give the full SHA of that commit on a line of its own as `ANSWER: <sha>`.
