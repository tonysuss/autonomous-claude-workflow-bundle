package main

import (
	"bytes"
	"testing"
)

func TestGet(t *testing.T) {
	var out, errb bytes.Buffer
	if code := run([]string{"get", "../../testdata/app.conf", "server.title"}, &out, &errb); code != 0 {
		t.Fatalf("exit %d: %s", code, errb.String())
	}
	if out.String() != "My App\n" {
		t.Errorf("got %q", out.String())
	}
}

func TestDump(t *testing.T) {
	var out, errb bytes.Buffer
	if code := run([]string{"dump", "../../testdata/app.conf"}, &out, &errb); code != 0 {
		t.Fatalf("exit %d: %s", code, errb.String())
	}
	want := "name=demo\nserver.host=example.org\nserver.port=8080\nserver.timeout=1m30s\nserver.title=My App\nfeatures.beta=off\nfeatures.search=yes\n"
	if out.String() != want {
		t.Errorf("got %q, want %q", out.String(), want)
	}
}

func TestUsage(t *testing.T) {
	var out, errb bytes.Buffer
	if code := run([]string{"frobnicate"}, &out, &errb); code != 2 {
		t.Errorf("exit %d, want 2", code)
	}
}
