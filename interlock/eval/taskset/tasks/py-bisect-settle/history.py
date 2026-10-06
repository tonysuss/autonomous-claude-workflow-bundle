"""The investigation repository's history, built on top of repos/tally.

Each commit is (message, culprit, edits). An edit is ("write", path, text) or
("replace", path, old, new); `old` must occur exactly once. The first commit
imports repos/tally unchanged. Exactly one commit is the culprit.
"""

REPRO = """#!/bin/sh
# Reported: settle prints a transfer of 0.00 for the trip example.
python3 - <<'PY'
from tally.ledger import Entry, Ledger

ledger = Ledger([
    Entry("2026-06-01", "Ann", 9000, ["Ann", "Bea", "Cal"]),
    Entry("2026-06-02", "Bea", 4000, ["Bea", "Cal"]),
    Entry("2026-06-02", "Cal", 12000, ["Ann", "Bea", "Cal", "Dan"]),
    Entry("2026-06-03", "Dan", -1200, ["Ann", "Dan"]),
])
transfers = ledger.settle()
zero = [t for t in transfers if t[2] == 0]
assert not zero, f"zero-amount transfers: {zero}"
assert transfers == [("Bea", "Cal", 4000), ("Dan", "Ann", 3600)], transfers
print("ok: no zero-amount transfers")
PY
"""

COMMITS = [
    ("Import tally", False, []),
    (
        "Add a weekend example",
        False,
        [
            (
                "write",
                "examples/weekend.csv",
                "date,payer,amount,participants,memo\n"
                "2026-07-04,Eve,60.00,Eve;Fay,fuel\n"
                "2026-07-05,Fay,30.00,Eve;Fay;Gus,lunch\n",
            )
        ],
    ),
    (
        "ledger: break settle ties by name",
        False,
        [
            ("replace", "tally/ledger.py", "key=lambda d: -d[0])", "key=lambda d: (-d[0], d[1]))"),
            ("replace", "tally/ledger.py", "key=lambda c: -c[0])", "key=lambda c: (-c[0], c[1]))"),
        ],
    ),
    (
        "money: document the sign of formatted amounts",
        False,
        [
            (
                "replace",
                "tally/money.py",
                '    """Formats cents as a decimal string: 1250 -> "12.50", -5 -> "-0.05"."""\n',
                '    """Formats cents as a decimal string: 1250 -> "12.50", -5 -> "-0.05".\n\n'
                "    The sign goes before the whole amount, so a refund of five cents reads\n"
                '    "-0.05", never "0.-5".\n    """\n',
            )
        ],
    ),
    (
        "cli: add --version",
        False,
        [
            (
                "replace",
                "tally/__init__.py",
                '"""tally: split shared expenses and settle up."""\n',
                '"""tally: split shared expenses and settle up."""\n\n__version__ = "0.3.0"\n',
            ),
            ("replace", "tally/cli.py", "from tally import report\n", "from tally import __version__, report\n"),
            (
                "replace",
                "tally/cli.py",
                '    sub = parser.add_subparsers(dest="command", required=True)\n',
                '    parser.add_argument("--version", action="version", version=f"tally {__version__}")\n'
                '    sub = parser.add_subparsers(dest="command", required=True)\n',
            ),
        ],
    ),
    (
        "ledger: tidy settle loop",
        True,
        [
            (
                "replace",
                "tally/ledger.py",
                "            if amount > 0:\n"
                "                transfers.append((debtors[i][1], creditors[j][1], amount))\n",
                "            transfers.append((debtors[i][1], creditors[j][1], amount))\n",
            ),
            (
                "replace",
                "tally/ledger.py",
                "            if creditors[j][0] == 0:\n                j += 1\n",
                "            elif creditors[j][0] == 0:\n                j += 1\n",
            ),
        ],
    ),
    (
        "report: say 'nothing to settle' when every balance is zero",
        False,
        [("replace", "tally/report.py", 'return "all settled\\n"', 'return "nothing to settle\\n"')],
    ),
    (
        "README: describe how settle pairs debts",
        False,
        [
            (
                "replace",
                "README.md",
                "`settle` prints the transfers that bring every balance to zero.\n",
                "`settle` prints the transfers that bring every balance to zero. It pairs\n"
                "the largest debt with the largest credit first, which keeps the number of\n"
                "transfers small.\n",
            )
        ],
    ),
    (
        "csvio: skip rows whose fields are all blank",
        False,
        [
            (
                "replace",
                "tally/csvio.py",
                "        for line, row in enumerate(reader, start=2):\n",
                "        for line, row in enumerate(reader, start=2):\n"
                '            if not any(isinstance(v, str) and v.strip() for v in row.values()):\n'
                "                continue\n",
            )
        ],
    ),
    ("Add a reproduction for the 0.00 transfer report", False, [("write", "checks/settle-repro.sh", REPRO)]),
]
