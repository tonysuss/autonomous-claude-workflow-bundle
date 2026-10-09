"""Text output for the command line."""

from tally.money import format_cents


def _readable(cents):
    """format_cents with thousands separators, for people to read."""
    text = format_cents(cents)
    sign = "-" if text.startswith("-") else ""
    whole, frac = text.lstrip("-").split(".")
    return f"{sign}{int(whole):,}.{frac}"


def render_balances(balances):
    """One line per person, in first-seen order, amounts right-aligned."""
    if not balances:
        return "no entries\n"
    width = max(len(name) for name in balances)
    return "".join(f"{name:<{width}}  {_readable(cents):>10}\n" for name, cents in balances.items())


def render_transfers(transfers):
    """One line per transfer: debtor -> creditor amount."""
    if not transfers:
        return "all settled\n"
    return "".join(f"{debtor} -> {creditor}  {_readable(cents)}\n" for debtor, creditor, cents in transfers)
