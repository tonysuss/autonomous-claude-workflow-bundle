package kvconf

// Hidden acceptance checks for go-env-expand. Each test is one stated
// requirement from the task's prompt.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func hiddenUnset(t *testing.T, name string) {
	t.Helper()
	t.Setenv(name, "")
	os.Unsetenv(name)
}

func hiddenGet(t *testing.T, c *Config, section, key string) string {
	t.Helper()
	v, err := c.Get(section, key)
	if err != nil {
		t.Fatalf("Get(%q, %q): %v", section, key, err)
	}
	return v
}

func hiddenParse(t *testing.T, text string) *Config {
	t.Helper()
	c, err := Parse(strings.NewReader(text))
	if err != nil {
		t.Fatalf("Parse(%q): %v", text, err)
	}
	return c
}

func TestHiddenBraceReferences(t *testing.T) {
	t.Setenv("KV_HOST", "db.local")
	c := hiddenParse(t, "[db]\nhost = ${KV_HOST}\nurl = \"postgres://${KV_HOST}/app main\"\n")
	if v := hiddenGet(t, c, "db", "host"); v != "db.local" {
		t.Errorf("host = %q", v)
	}
	if v := hiddenGet(t, c, "db", "url"); v != "postgres://db.local/app main" {
		t.Errorf("url = %q", v)
	}
}

func TestHiddenFallbacks(t *testing.T) {
	hiddenUnset(t, "KV_MISSING")
	t.Setenv("KV_EMPTY", "")
	t.Setenv("KV_USER", "ann")
	c := hiddenParse(t, "a = ${KV_MISSING:-guest}\nb = ${KV_EMPTY:-guest}\nc = ${KV_USER:-guest}\n")
	for key, want := range map[string]string{"a": "guest", "b": "guest", "c": "ann"} {
		if v := hiddenGet(t, c, "", key); v != want {
			t.Errorf("%s = %q, want %q", key, v, want)
		}
	}
}

func TestHiddenDollarRules(t *testing.T) {
	c := hiddenParse(t, "price = 5$$\ncost = $5\nhome = $HOME\nend = 3$\n")
	for key, want := range map[string]string{"price": "5$", "cost": "$5", "home": "$HOME", "end": "3$"} {
		if v := hiddenGet(t, c, "", key); v != want {
			t.Errorf("%s = %q, want %q", key, v, want)
		}
	}
}

func TestHiddenUndefinedVariableError(t *testing.T) {
	hiddenUnset(t, "KV_NOPE")
	_, err := Parse(strings.NewReader("a = 1\nb = ${KV_NOPE}\n"))
	if err == nil || err.Error() != `kvconf: line 2: undefined variable "KV_NOPE"` {
		t.Errorf("error = %v", err)
	}
}

func TestHiddenNoExpandOption(t *testing.T) {
	hiddenUnset(t, "KV_NOPE")
	c, err := ParseWithOptions(strings.NewReader("x = ${KV_NOPE}\ny = $$\n"), ParseOptions{NoExpand: true})
	if err != nil {
		t.Fatal(err)
	}
	if v := hiddenGet(t, c, "", "x"); v != "${KV_NOPE}" {
		t.Errorf("x = %q", v)
	}
	if v := hiddenGet(t, c, "", "y"); v != "$$" {
		t.Errorf("y = %q", v)
	}
	t.Setenv("KV_HOST", "h")
	c, err = ParseWithOptions(strings.NewReader("x = ${KV_HOST}\n"), ParseOptions{})
	if err != nil || hiddenGet(t, c, "", "x") != "h" {
		t.Errorf("zero ParseOptions should expand: %v", err)
	}
}

func TestHiddenLoadExpands(t *testing.T) {
	t.Setenv("KV_ROOT", "/srv")
	path := filepath.Join(t.TempDir(), "app.conf")
	if err := os.WriteFile(path, []byte("[paths]\ndata = ${KV_ROOT}/data\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	c, err := Load(path)
	if err != nil {
		t.Fatal(err)
	}
	if v := hiddenGet(t, c, "paths", "data"); v != "/srv/data" {
		t.Errorf("data = %q", v)
	}
}
