"""Command line: python3 -m tally {balances,settle,export} FILE"""

import argparse
import sys

from tally import report
from tally.csvio import load_ledger
from tally.export import export_entries
from tally.ledger import parse_date


def _date(text):
    try:
        return parse_date(text)
    except ValueError as e:
        raise argparse.ArgumentTypeError(str(e)) from None


def main(argv=None):
    parser = argparse.ArgumentParser(prog="tally", description="Split shared expenses.")
    sub = parser.add_subparsers(dest="command", required=True)
    for name, help_text in (
        ("balances", "show what each person is owed (+) or owes (-)"),
        ("settle", "list the transfers that settle every balance"),
        ("export", "print the entries as normalized CSV"),
    ):
        p = sub.add_parser(name, help=help_text)
        p.add_argument("file", help="CSV file with columns date,payer,amount,participants,memo")
        if name != "export":
            p.add_argument("--since", type=_date, help="only entries on or after this YYYY-MM-DD date")
            p.add_argument("--until", type=_date, help="only entries on or before this YYYY-MM-DD date")
    args = parser.parse_args(argv)
    try:
        ledger = load_ledger(args.file)
        if getattr(args, "since", None) or getattr(args, "until", None):
            ledger = ledger.between(args.since, args.until)
    except (OSError, ValueError) as e:
        print(f"tally: {e}", file=sys.stderr)
        return 1
    if args.command == "balances":
        sys.stdout.write(report.render_balances(ledger.balances()))
    elif args.command == "settle":
        sys.stdout.write(report.render_transfers(ledger.settle()))
    else:
        sys.stdout.write(export_entries(ledger.entries))
    return 0
