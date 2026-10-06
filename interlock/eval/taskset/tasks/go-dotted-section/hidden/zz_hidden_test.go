package kvconf

// Hidden acceptance checks for go-dotted-section. The prompt states the
// naming rule; the README says Lookup takes the command line's names, so the
// rule holds there too: the same bug at a second call site.

import (
	"strings"
	"testing"
)

const hiddenDotted = "name = demo\n[server]\nport = 8080\n[db.eu]\nhost = db1.example.org\n[a.b.c]\nd = deep\n"

func hiddenDottedConfig(t *testing.T) *Config {
	t.Helper()
	c, err := Parse(strings.NewReader(hiddenDotted))
	if err != nil {
		t.Fatal(err)
	}
	return c
}

func TestHiddenLookupDottedSection(t *testing.T) {
	c := hiddenDottedConfig(t)
	for name, want := range map[string]string{"db.eu.host": "db1.example.org", "a.b.c.d": "deep"} {
		if v, err := c.Lookup(name); err != nil || v != want {
			t.Errorf("Lookup(%q) = %q, %v; want %q", name, v, err, want)
		}
	}
}

func TestHiddenLookupPlainNames(t *testing.T) {
	c := hiddenDottedConfig(t)
	if v, err := c.Lookup("server.port"); err != nil || v != "8080" {
		t.Errorf("Lookup(server.port) = %q, %v", v, err)
	}
	if v, err := c.Lookup("name"); err != nil || v != "demo" {
		t.Errorf("Lookup(name) = %q, %v", v, err)
	}
	if _, err := c.Lookup("db.eu.missing"); err == nil || err.Error() != `kvconf: [db.eu]: no key "missing"` {
		t.Errorf("Lookup(db.eu.missing) error = %v", err)
	}
}
