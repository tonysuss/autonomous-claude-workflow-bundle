`kvconf get` cannot read a key in a section whose name contains a dot. With this file:

```
[db.eu]
host = db1.example.org
```

`go run ./cmd/kvconf get app.conf db.eu.host` prints `kvconf: no section "db"` instead of `db1.example.org`.

Section names may contain dots; keys never do. So in a name of the form `SECTION.KEY`, the key is the part after the last dot: `db.eu.host` is the key `host` in the section `db.eu`. A name without a dot is a key in the unnamed section.

`checks/dotted.sh` reproduces the problem. The tests run with `go test ./...`.
