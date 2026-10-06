Large amounts are hard to read in the output of `python3 -m tally balances` and `python3 -m tally settle`: a balance prints as `-121810.78`. Print the amounts in both commands with thousands separators, `-121,810.78`. Nothing else about the program's behaviour should change.

`checks/thousands.sh` checks `balances` for `examples/big.csv`. The tests run with `python3 -m unittest discover -s tests -q`.
