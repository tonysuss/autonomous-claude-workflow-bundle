Configuration values should be able to use environment variables.

- In unquoted and double-quoted values, `${NAME}` is replaced by the value of the environment variable `NAME`, and `${NAME:-fallback}` by its value, or by `fallback` when `NAME` is unset or empty.
- `$$` stands for a literal `$`. A `$` followed by anything other than `{` or `$` is kept as it is.
- Referring to an unset variable without a fallback is an error: parsing fails with `kvconf: line N: undefined variable "NAME"`, where N is the line number.
- `Parse` and `Load` expand values. Add this API so callers can keep values literally:

  ```go
  type ParseOptions struct{ NoExpand bool }

  func ParseWithOptions(r io.Reader, opts ParseOptions) (*Config, error)
  ```

  `Parse(r)` behaves like `ParseWithOptions(r, ParseOptions{})`.
- `kvconf get` and `kvconf dump` accept a `--no-expand` flag, given before the file name, that keeps values literally: `kvconf get --no-expand app.conf paths.data`.

`checks/expand.sh` checks the basic case. The tests run with `go test ./...`.
