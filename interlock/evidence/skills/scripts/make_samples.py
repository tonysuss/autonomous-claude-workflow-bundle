"""Writes the two sample repositories for the live guided runs (git is run separately)."""
import os

ROOT = os.path.dirname(os.path.abspath(__file__))

DURATIONS = '''"""Parse human durations like "90s", "5m" or "1h30m" into seconds."""

UNITS = {"h": 3600, "m": 60, "s": 1}


def parse_duration(text):
    """Return the number of seconds in a duration such as "1h30m" or "45s"."""
    total = 0
    number = ""
    for ch in text.strip():
        if ch.isdigit():
            number += ch
        elif ch in UNITS:
            if not number:
                raise ValueError(f"missing number before {ch!r} in {text!r}")
            total = int(number) * UNITS[ch]
            number = ""
        else:
            raise ValueError(f"unknown unit {ch!r} in {text!r}")
    if number:
        raise ValueError(f"missing unit after {number!r} in {text!r}")
    return total
'''

TEST_DURATIONS = '''import unittest

from durations import parse_duration


class ParseDurationTest(unittest.TestCase):
    def test_seconds(self):
        self.assertEqual(parse_duration("45s"), 45)

    def test_minutes(self):
        self.assertEqual(parse_duration("5m"), 300)

    def test_rejects_unknown_units(self):
        with self.assertRaises(ValueError):
            parse_duration("3d")


if __name__ == "__main__":
    unittest.main()
'''

CACHE_V1 = '''import time


class TTLCache:
    """A small in-memory cache whose entries expire after `ttl` seconds."""

    def __init__(self, ttl=60, clock=time.monotonic):
        self.ttl = ttl
        self.clock = clock
        self._items = {}

    def put(self, key, value):
        self._items[key] = (value, self.clock())

    def get(self, key, default=None):
        item = self._items.get(key)
        if item is None:
            return default
        value, stored = item
        if self.clock() - stored >= self.ttl:
            del self._items[key]
            return default
        return value
'''

CACHE_V2 = CACHE_V1.replace("stored >= self.ttl", "stored > self.ttl")


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


bug = os.path.join(ROOT, "bugfix-repo")
write(os.path.join(bug, "durations.py"), DURATIONS)
write(os.path.join(bug, "tests", "__init__.py"), "")
write(os.path.join(bug, "tests", "test_durations.py"), TEST_DURATIONS)
write(os.path.join(bug, ".gitignore"), "__pycache__/\n")

inv = os.path.join(ROOT, "investigation-repo")
write(os.path.join(inv, "cache.py"), CACHE_V1)
write(os.path.join(inv, ".gitignore"), "__pycache__/\n")
with open(os.path.join(ROOT, "cache_v2.py"), "w") as f:
    f.write(CACHE_V2)
print("ok")
