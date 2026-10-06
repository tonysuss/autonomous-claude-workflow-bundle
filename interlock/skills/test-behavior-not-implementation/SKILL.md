# Test behavior, not implementation

A test calls the code the way its users do and asserts the result they observe against a literal expected value.

**The check:** would the test still pass if every function it imports returned nothing? If so, it cannot fail for a defect. Rewrite the assertion or delete the test.

Shapes that pass no matter what the code does:

- **Weak or no assertion:** only "is defined", "is truthy", "does not throw".
- **Mocks only:** asserting that something was called, not what came out.
- **Self-referential:** the expected value comes from the code under test, as in `assert f(a) == f(a)`.
- **Constant pin:** restating a constant or a prompt string the code contains.
- **Fixture asserts fixture:** the assertion reads data the test built, and the subject never runs.

The fix: call the subject with one concrete input and assert the literal output or the visible effect: `assert export([1, 2, 3], sink) == 3 and sink.rows == [1, 2, 3]`.

For a bug fix, the new test must fail on the input snapshot and pass after the fix. If it passes on both, it does not test the bug.
