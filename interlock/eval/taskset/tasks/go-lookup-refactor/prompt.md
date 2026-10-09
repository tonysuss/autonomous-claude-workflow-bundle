`getters.go` repeats the same section-and-key lookup in `Get`, `GetInt`, `GetBool` and `GetDuration`. Move it into one unexported method,

```go
func (c *Config) lookup(section, key string) (string, error)
```

and make all four getters use it, so that none of them reads `c.sections` directly any more.

This is a pure refactor. The exported API, every returned value and every error message must stay exactly as they are now. The tests run with `go test ./...`.
