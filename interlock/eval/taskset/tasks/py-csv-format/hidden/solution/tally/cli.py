"""Command line: python3 -m tally {balances,settle,export} [--format text|csv] FILE"""

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
        if name != "export":
            p.add_argument("--format", choices=["text", "csv"], default="text", help="output format")
    args = parser.parse_args(argv)
    try:
        ledger = load_ledger(args.file)
    except (OSError, ValueError) as e:
        print(f"tally: {e}", file=sys.stderr)
        return 1
    if args.command == "export":
        sys.stdout.write(export_entries(ledger.entries))
        return 0
    csv_out = args.format == "csv"
    if args.command == "balances":
        render = report.render_balances_csv if csv_out else report.render_balances
        sys.stdout.write(render(ledger.balances()))
    else:
        render = report.render_transfers_csv if csv_out else report.render_transfers
        sys.stdout.write(render(ledger.settle()))
    return 0
