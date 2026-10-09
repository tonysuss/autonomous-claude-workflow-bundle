# tally

Split shared expenses and settle up.

```
python3 -m tally balances examples/trip.csv
python3 -m tally settle examples/trip.csv
python3 -m tally export examples/trip.csv
```

A ledger is a CSV file with the columns `date,payer,amount,participants,memo`.
Participants are separated by semicolons. Amounts are decimals with at most two
places; a negative amount is a refund. Each expense is split evenly among its
participants, to the cent.

`balances` prints what each person is owed (positive) or owes (negative).
`settle` prints the transfers that bring every balance to zero.
`export` prints the entries as normalized CSV, for other programs to read.

## Development

```
python3 -m unittest discover -s tests -q
```
