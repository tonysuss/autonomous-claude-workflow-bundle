package kvconf

// Hidden acceptance checks for go-lookup-refactor: behaviour and messages
// must be exactly what they were before the refactor.

import (
	"strings"
	"testing"
	"time"
)

// The exported API is unchanged.
var (
	_ func(*Config, string, string) (string, error)        = (*Config).Get
	_ func(*Config, string, string) (int, error)           = (*Config).GetInt
	_ func(*Config, string, string) (bool, error)          = (*Config).GetBool
	_ func(*Config, string, string) (time.Duration, error) = (*Config).GetDuration
	_ func(*Config) []string                               = (*Config).Sections
	_ func(*Config, string) []string                       = (*Config).Keys
)

const hiddenSample = "top = 1\n[limits]\ncount = 1_000\nbad = lots\nflag = On\noff = 0\nwait = 90\nlong = 1m30s\n"

func hiddenConfig(t *testing.T) *Config {
	t.Helper()
	c, err := Parse(strings.NewReader(hiddenSample))
	if err != nil {
		t.Fatal(err)
	}
	return c
}

func wantErr(t *testing.T, what string, err error, want string) {
	t.Helper()
	if err == nil || err.Error() != want {
		t.Errorf("%s: error = %v, want %q", what, err, want)
	}
}

func TestHiddenMissingSectionMessages(t *testing.T) {
	c := hiddenConfig(t)
	const want = `kvconf: no section "nope"`
	_, err := c.Get("nope", "x")
	wantErr(t, "Get", err, want)
	_, err = c.GetInt("nope", "x")
	wantErr(t, "GetInt", err, want)
	_, err = c.GetBool("nope", "x")
	wantErr(t, "GetBool", err, want)
	_, err = c.GetDuration("nope", "x")
	wantErr(t, "GetDuration", err, want)
}

func TestHiddenMissingKeyMessages(t *testing.T) {
	c := hiddenConfig(t)
	_, err := c.Get("limits", "x")
	wantErr(t, "Get", err, `kvconf: [limits]: no key "x"`)
	_, err = c.GetInt("limits", "x")
	wantErr(t, "GetInt", err, `kvconf: [limits]: no key "x"`)
	_, err = c.GetDuration("limits", "x")
	wantErr(t, "GetDuration", err, `kvconf: [limits]: no key "x"`)
	_, err = c.Get("", "x")
	wantErr(t, "Get top level", err, `kvconf: top level: no key "x"`)
}

func TestHiddenGetBoolMissingKeyMessage(t *testing.T) {
	c := hiddenConfig(t)
	_, err := c.GetBool("limits", "x")
	wantErr(t, "GetBool", err, `kvconf: [limits]: key "x" not set`)
	_, err = c.GetBool("", "x")
	wantErr(t, "GetBool top level", err, `kvconf: top level: key "x" not set`)
}

func TestHiddenBadValueMessages(t *testing.T) {
	c := hiddenConfig(t)
	_, err := c.GetInt("limits", "bad")
	wantErr(t, "GetInt", err, `kvconf: limits.bad: "lots" is not an integer`)
	_, err = c.GetBool("limits", "bad")
	wantErr(t, "GetBool", err, `kvconf: limits.bad: "lots" is not a boolean`)
	_, err = c.GetDuration("limits", "bad")
	wantErr(t, "GetDuration", err, `kvconf: limits.bad: "lots" is not a duration`)
}

func TestHiddenValues(t *testing.T) {
	c := hiddenConfig(t)
	if v, err := c.Get("", "top"); err != nil || v != "1" {
		t.Errorf("Get(top) = %q, %v", v, err)
	}
	if n, err := c.GetInt("limits", "count"); err != nil || n != 1000 {
		t.Errorf("GetInt(count) = %d, %v", n, err)
	}
	if b, err := c.GetBool("limits", "flag"); err != nil || !b {
		t.Errorf("GetBool(flag) = %v, %v", b, err)
	}
	if b, err := c.GetBool("limits", "off"); err != nil || b {
		t.Errorf("GetBool(off) = %v, %v", b, err)
	}
	if d, err := c.GetDuration("limits", "wait"); err != nil || d != 90*time.Second {
		t.Errorf("GetDuration(wait) = %v, %v", d, err)
	}
	if d, err := c.GetDuration("limits", "long"); err != nil || d != 90*time.Second {
		t.Errorf("GetDuration(long) = %v, %v", d, err)
	}
}
