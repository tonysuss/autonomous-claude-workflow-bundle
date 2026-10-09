Add `--since DATE` and `--until DATE` options to `python3 -m tally balances` and `python3 -m tally settle`, so that part of a trip can be looked at on its own. DATE is written `YYYY-MM-DD`.

- Only entries dated on or after `--since` and on or before `--until` count. Either option may be given alone.
- A DATE that is not a valid `YYYY-MM-DD` date is rejected by the argument parser, which exits with status 2.
- When `--since` or `--until` is given, every entry's date must be a valid `YYYY-MM-DD` date. Otherwise the command fails with status 1 and an error that names the file and the line, like `tally: trip.csv:3: invalid date '2026-13-01'`.
- Without these options, nothing changes.

`checks/date-filter.sh` checks `balances --since 2026-06-02` for `examples/trip.csv`. The tests run with `python3 -m unittest discover -s tests -q`.
