`kvconf get` fails on a value that has a `#` inside double quotes. With this file:

```
[app]
title = "Issue #42: retry"
```

`go run ./cmd/kvconf get app.conf app.title` prints `kvconf: line 2: unterminated quoted value` instead of `Issue #42: retry`.

A `#` inside a double-quoted value is part of the value. The other quoting rules in the README must keep working:

- an unquoted value still ends at the first `#`: `port = 8080 # http` gives `8080`;
- after the closing quote, `# ...` is a comment: `title = "x" # note` gives `x`;
- inside quotes, `\"` is a double quote and `\\` is a single backslash: `path = "C:\\temp"` gives `C:\temp`.

`checks/quoted-hash.sh` reproduces the problem. The tests run with `go test ./...`.
