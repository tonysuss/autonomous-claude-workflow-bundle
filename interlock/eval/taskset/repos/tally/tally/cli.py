"""Command line: python3 -m tally {balances,settle,export} FILE"""

import argparse
import sys

from tally import report
from tally.csvio import load_ledger
from tally.export import export_entries


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
    args = parser.parse_args(argv)
    try:
        ledger = load_ledger(args.file)
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
