"""Money helpers. Amounts are whole cents held in ints, never floats."""


def parse_amount(text):
    """Parses a decimal amount such as "12.50", "-3.2" or "1,234.56" into cents.

    Thousands separators are ignored. At most two decimal places are allowed.
    """
    s = text.strip().replace(",", "")
    sign = 1
    if s[:1] in ("+", "-"):
        sign = -1 if s[0] == "-" else 1
        s = s[1:]
    whole, dot, frac = s.partition(".")
    if not (whole.isdigit() or (whole == "" and frac.isdigit())):
        raise ValueError(f"not an amount: {text!r}")
    if frac and not frac.isdigit():
        raise ValueError(f"not an amount: {text!r}")
    if len(frac) > 2:
        raise ValueError(f"too many decimal places: {text!r}")
    return sign * (int(whole or "0") * 100 + int(frac.ljust(2, "0")))


def format_cents(cents):
    """Formats cents as a decimal string: 1250 -> "12.50", -5 -> "-0.05"."""
    sign = "-" if cents < 0 else ""
    whole, frac = divmod(abs(cents), 100)
    return f"{sign}{whole}.{frac:02d}"


def split_evenly(total, n):
    """Splits `total` cents into `n` shares that differ by at most one cent and
    sum to `total`. Earlier shares take the extra cents. A negative total
    mirrors the positive case: split_evenly(-100, 3) == [-34, -33, -33].
    """
    if n <= 0:
        raise ValueError("n must be positive")
    sign = -1 if total < 0 else 1
    base, extra = divmod(abs(total), n)
    return [sign * (base + (1 if i < extra else 0)) for i in range(n)]
