"""Reading ledgers from CSV files.

Columns: date, payer, amount, participants, memo. Participants are separated
by semicolons, so a name may contain a comma when the field is quoted.
"""

import csv

from tally.ledger import Entry, Ledger
from tally.money import parse_amount

REQUIRED = ["date", "payer", "amount", "participants"]


def read_entries(path):
    with open(path, newline="") as f:
        reader = csv.DictReader(f)
        missing = [c for c in REQUIRED if c not in (reader.fieldnames or [])]
        if missing:
            raise ValueError(f"{path}: missing columns: {', '.join(missing)}")
        entries = []
        for line, row in enumerate(reader, start=2):
            try:
                amount = parse_amount(row["amount"])
            except ValueError as e:
                raise ValueError(f"{path}:{line}: {e}") from None
            people = [p.strip() for p in row["participants"].split(";") if p.strip()]
            if not people:
                raise ValueError(f"{path}:{line}: no participants")
            memo = (row.get("memo") or "").strip()
            entries.append(Entry(row["date"].strip(), row["payer"].strip(), amount, people, memo))
        return entries


def load_ledger(path):
    return Ledger(read_entries(path))
