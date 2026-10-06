# kvconf

A small reader for INI-style configuration files, and a command to query them.

```
# a comment
name = demo

[server]
port = 8080          # an inline comment
title = "My App"     # quotes keep spaces; \" and \\ are escapes
```

```go
cfg, err := kvconf.Load("app.conf")
port, err := cfg.GetInt("server", "port")
```

```
go run ./cmd/kvconf get testdata/app.conf server.title
go run ./cmd/kvconf dump testdata/app.conf
```

Getters: `Get`, `GetInt`, `GetBool`, `GetDuration`.

## Development

```
go test ./...
```
