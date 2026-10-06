"""Money helpers. Amounts are whole cents held in ints, never floats."""


def parse_amount(text):
    """Parses a decimal amount such as "12.50" or "1,234.56" into cents.

    Thousands separators are ignored.
    """
    s = text.strip().replace(",", "")
    if "." in s:
        whole, frac = s.split(".", 1)
        return int(whole) * 100 + int(frac)
    return int(s) * 100


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
