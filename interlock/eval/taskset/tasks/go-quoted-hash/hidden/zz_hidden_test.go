package kvconf

// Hidden acceptance checks for go-quoted-hash. Each test is one stated
// requirement from the task's prompt.

import (
	"strings"
	"testing"
)

func hiddenValue(t *testing.T, text, section, key string) string {
	t.Helper()
	c, err := Parse(strings.NewReader(text))
	if err != nil {
		t.Fatalf("Parse(%q): %v", text, err)
	}
	v, err := c.Get(section, key)
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	return v
}

func TestHiddenHashInsideQuotes(t *testing.T) {
	if v := hiddenValue(t, "[app]\ntitle = \"Issue #42: retry\"\n", "app", "title"); v != "Issue #42: retry" {
		t.Errorf("got %q", v)
	}
	if v := hiddenValue(t, "x = \"#\"\n", "", "x"); v != "#" {
		t.Errorf("got %q", v)
	}
}

func TestHiddenUnquotedValueEndsAtHash(t *testing.T) {
	if v := hiddenValue(t, "port = 8080 # http\n", "", "port"); v != "8080" {
		t.Errorf("got %q", v)
	}
	if v := hiddenValue(t, "a = b#c\n", "", "a"); v != "b" {
		t.Errorf("got %q", v)
	}
}

func TestHiddenCommentAfterClosingQuote(t *testing.T) {
	if v := hiddenValue(t, "title = \"x\" # note\n", "", "title"); v != "x" {
		t.Errorf("got %q", v)
	}
	if v := hiddenValue(t, "title = \"a # b\"   # c \"d\"\n", "", "title"); v != "a # b" {
		t.Errorf("got %q", v)
	}
}

func TestHiddenEscapes(t *testing.T) {
	if v := hiddenValue(t, `path = "C:\\temp"`+"\n", "", "path"); v != `C:\temp` {
		t.Errorf("got %q", v)
	}
	if v := hiddenValue(t, `q = "say \"hi\" # now"`+"\n", "", "q"); v != `say "hi" # now` {
		t.Errorf("got %q", v)
	}
	if v := hiddenValue(t, `e = "end\\"`+"\n", "", "e"); v != `end\` {
		t.Errorf("got %q", v)
	}
}
