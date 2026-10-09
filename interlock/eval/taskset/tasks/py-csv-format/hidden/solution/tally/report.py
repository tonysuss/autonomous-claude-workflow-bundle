"""Output for the command line, as aligned text or CSV."""

import csv
import io

from tally.money import format_cents


def render_balances(balances):
    """One line per person, in first-seen order, amounts right-aligned."""
    if not balances:
        return "no entries\n"
    width = max(len(name) for name in balances)
    return "".join(f"{name:<{width}}  {format_cents(cents):>10}\n" for name, cents in balances.items())


def render_transfers(transfers):
    """One line per transfer: debtor -> creditor amount."""
    if not transfers:
        return "all settled\n"
    return "".join(f"{debtor} -> {creditor}  {format_cents(cents)}\n" for debtor, creditor, cents in transfers)


def _csv(header, rows):
    out = io.StringIO()
    writer = csv.writer(out, lineterminator="\n")
    writer.writerow(header)
    writer.writerows(rows)
    return out.getvalue()


def render_balances_csv(balances):
    return _csv(["person", "balance"], [(name, format_cents(cents)) for name, cents in balances.items()])


def render_transfers_csv(transfers):
    return _csv(["from", "to", "amount"], [(d, c, format_cents(cents)) for d, c, cents in transfers])
