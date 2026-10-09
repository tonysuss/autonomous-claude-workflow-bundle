package kvconf

import (
	"testing"
	"time"
)

const sample = `
[limits]
count = 1_000
bad = lots
search = Yes
beta = off
timeout = 1m30s
grace = 90
never = soon
`

func TestGetInt(t *testing.T) {
	c := mustParse(t, sample)
	if n, err := c.GetInt("limits", "count"); err != nil || n != 1000 {
		t.Errorf("GetInt(count) = %d, %v", n, err)
	}
	if _, err := c.GetInt("limits", "bad"); err == nil {
		t.Error("GetInt(bad) should fail")
	}
}

func TestGetBool(t *testing.T) {
	c := mustParse(t, sample)
	if b, err := c.GetBool("limits", "search"); err != nil || !b {
		t.Errorf("GetBool(search) = %v, %v", b, err)
	}
	if b, err := c.GetBool("limits", "beta"); err != nil || b {
		t.Errorf("GetBool(beta) = %v, %v", b, err)
	}
	if _, err := c.GetBool("limits", "missing"); err == nil {
		t.Error("GetBool(missing) should fail")
	}
}

func TestGetDuration(t *testing.T) {
	c := mustParse(t, sample)
	for key, want := range map[string]time.Duration{"timeout": 90 * time.Second, "grace": 90 * time.Second} {
		if d, err := c.GetDuration("limits", key); err != nil || d != want {
			t.Errorf("GetDuration(%s) = %v, %v", key, d, err)
		}
	}
	if _, err := c.GetDuration("limits", "never"); err == nil {
		t.Error("GetDuration(never) should fail")
	}
}

func TestGetErrors(t *testing.T) {
	c := mustParse(t, sample)
	if _, err := c.Get("nope", "x"); err == nil || err.Error() != `kvconf: no section "nope"` {
		t.Errorf("Get(nope, x) error = %v", err)
	}
	if _, err := c.Get("limits", "x"); err == nil || err.Error() != `kvconf: [limits]: no key "x"` {
		t.Errorf("Get(limits, x) error = %v", err)
	}
}
