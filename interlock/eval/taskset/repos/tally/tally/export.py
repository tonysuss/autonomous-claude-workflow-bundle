"""Machine-readable export: a ledger's entries as normalized CSV, in the same
columns `csvio` reads, so the output can be loaded again."""

import csv
import io

from tally.money import format_cents


def export_entries(entries):
    out = io.StringIO()
    writer = csv.writer(out, lineterminator="\n")
    writer.writerow(["date", "payer", "amount", "participants", "memo"])
    for e in entries:
        writer.writerow([e.date, e.payer, format_cents(e.amount), ";".join(e.participants), e.memo])
    return out.getvalue()
