package kvconf

import (
	"reflect"
	"strings"
	"testing"
)

func mustParse(t *testing.T, text string) *Config {
	t.Helper()
	c, err := Parse(strings.NewReader(text))
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	return c
}

func get(t *testing.T, c *Config, section, key string) string {
	t.Helper()
	v, err := c.Get(section, key)
	if err != nil {
		t.Fatalf("Get(%q, %q): %v", section, key, err)
	}
	return v
}

func TestSectionsAndKeys(t *testing.T) {
	c, err := Load("testdata/app.conf")
	if err != nil {
		t.Fatal(err)
	}
	if got, want := c.Sections(), []string{"", "server", "features"}; !reflect.DeepEqual(got, want) {
		t.Errorf("Sections() = %q, want %q", got, want)
	}
	if got, want := c.Keys("server"), []string{"host", "port", "timeout", "title"}; !reflect.DeepEqual(got, want) {
		t.Errorf("Keys(server) = %q, want %q", got, want)
	}
	if got := get(t, c, "", "name"); got != "demo" {
		t.Errorf("name = %q", got)
	}
}

func TestValues(t *testing.T) {
	c := mustParse(t, "[s]\nport = 8080 # http\ntitle = \"My App\"\nquote = \"say \\\"hi\\\"\"\nempty =\n")
	for key, want := range map[string]string{"port": "8080", "title": "My App", "quote": `say "hi"`, "empty": ""} {
		if got := get(t, c, "s", key); got != want {
			t.Errorf("%s = %q, want %q", key, got, want)
		}
	}
}

func TestParseErrors(t *testing.T) {
	for text, want := range map[string]string{
		"[server\n":        "line 1: unterminated section header",
		"a = 1\nnovalue\n": "line 2: expected key = value",
		"= x\n":            "line 1: missing key",
		"[]\n":             "line 1: empty section name",
		"t = \"abc\n":      "line 1: unterminated quoted value",
	} {
		_, err := Parse(strings.NewReader(text))
		if err == nil || !strings.Contains(err.Error(), want) {
			t.Errorf("Parse(%q) error = %v, want it to contain %q", text, err, want)
		}
	}
}
