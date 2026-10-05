"""Exports rows to a sink, retrying once when a write fails."""


class TransientError(Exception):
    pass


class Sink:
    def __init__(self, fail_at=None):
        self.rows = []
        self.fail_at = fail_at
        self.calls = 0

    def write(self, row):
        self.calls += 1
        if self.fail_at is not None and self.calls == self.fail_at:
            raise TransientError("connection reset")
        self.rows.append(row)


def export(rows, sink, retries=1):
    """Write every row to the sink. On a transient failure, retry the batch."""
    for attempt in range(retries + 1):
        try:
            for row in rows:
                sink.write(row)
            return len(rows)
        except TransientError:
            if attempt == retries:
                raise
    return 0
