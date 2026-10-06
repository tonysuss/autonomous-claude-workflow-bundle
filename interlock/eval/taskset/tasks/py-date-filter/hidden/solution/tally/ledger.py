"""A ledger of shared expenses and the balances they leave."""

import datetime
import re
from dataclasses import dataclass, field

from tally.money import split_evenly

_DATE = re.compile(r"\d{4}-\d{2}-\d{2}")


def parse_date(text):
    """A YYYY-MM-DD date, or ValueError."""
    if not _DATE.fullmatch(text):
        raise ValueError(f"invalid date {text!r}")
    try:
        return datetime.date.fromisoformat(text)
    except ValueError:
        raise ValueError(f"invalid date {text!r}") from None


@dataclass
class Entry:
    date: str
    payer: str
    amount: int  # cents; negative for a refund
    participants: list = field(default_factory=list)  # names, in the order given
    memo: str = ""
    source: str = ""  # where the entry was read, as FILE:LINE


class Ledger:
    def __init__(self, entries=None):
        self.entries = []
        for e in entries or []:
            self.add(e)

    def add(self, entry):
        self.entries.append(entry)

    def between(self, since=None, until=None):
        """The entries dated from `since` to `until`, both inclusive."""
        kept = []
        for e in self.entries:
            try:
                day = parse_date(e.date)
            except ValueError as err:
                raise ValueError(f"{e.source}: {err}" if e.source else str(err)) from None
            if (since is None or day >= since) and (until is None or day <= until):
                kept.append(e)
        return Ledger(kept)

    def people(self):
        """Everyone in the ledger, in the order they first appear."""
        names = []
        for e in self.entries:
            for name in [e.payer, *e.participants]:
                if name not in names:
                    names.append(name)
        return names

    def balances(self):
        """Net cents per person: positive means the group owes them money."""
        totals = {name: 0 for name in self.people()}
        for e in self.entries:
            totals[e.payer] += e.amount
            shares = split_evenly(e.amount, len(e.participants))
            for name, share in zip(e.participants, shares):
                totals[name] -= share
        return totals

    def settle(self):
        """Transfers (debtor, creditor, cents) that bring every balance to zero.

        The largest debts are paired with the largest credits first, which
        keeps the number of transfers small.
        """
        balances = self.balances()
        debtors = sorted(([-b, n] for n, b in balances.items() if b < 0), key=lambda d: -d[0])
        creditors = sorted(([b, n] for n, b in balances.items() if b > 0), key=lambda c: -c[0])
        transfers = []
        i = j = 0
        while i < len(debtors) and j < len(creditors):
            amount = min(debtors[i][0], creditors[j][0])
            if amount > 0:
                transfers.append((debtors[i][1], creditors[j][1], amount))
            debtors[i][0] -= amount
            creditors[j][0] -= amount
            if debtors[i][0] == 0:
                i += 1
            if creditors[j][0] == 0:
                j += 1
        return transfers
