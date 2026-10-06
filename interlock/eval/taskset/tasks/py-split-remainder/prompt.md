Splitting an expense loses cents. `split_evenly(100, 3)` in `tally/money.py` returns `[33, 33, 33]`, which adds up to 99, so the balances for a ledger with such an expense no longer sum to zero.

Expected behaviour of `split_evenly(total, n)`:

- The shares always add up to `total`, and no two shares differ by more than one cent.
- The earlier shares (participants are in the order given) take the extra cents: `split_evenly(100, 3) == [34, 33, 33]`.
- A negative total (a refund) mirrors the positive case: `split_evenly(-100, 3) == [-34, -33, -33]`.
- `n` must be positive; otherwise `split_evenly` raises `ValueError`.

`checks/split-repro.sh` reproduces the problem. The tests run with `python3 -m unittest discover -s tests -q`.
