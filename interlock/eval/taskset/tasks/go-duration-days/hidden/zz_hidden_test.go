package kvconf

// Hidden acceptance checks for go-duration-days. The prompt asks for "the
// unit d" in GetDuration, which already accepts Go duration syntax: numbers
// with optional fractions, each with a unit, in sequence ("1m30s", "1.5h").
// A unit in that syntax composes with the others and takes fractions; the
// existing tests already use the composite "1m30s".

import (
	"strings"
	"testing"
	"time"
)

func hiddenDuration(t *testing.T, value string) (time.Duration, error) {
	t.Helper()
	c, err := Parse(strings.NewReader("[s]\nk = " + value + "\n"))
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	return c.GetDuration("s", "k")
}

func hiddenWant(t *testing.T, cases map[string]time.Duration) {
	t.Helper()
	for value, want := range cases {
		if d, err := hiddenDuration(t, value); err != nil || d != want {
			t.Errorf("GetDuration(%q) = %v, %v; want %v", value, d, err, want)
		}
	}
}

func TestHiddenDays(t *testing.T) {
	hiddenWant(t, map[string]time.Duration{"30d": 720 * time.Hour, "1d": 24 * time.Hour})
}

func TestHiddenDaysWithOtherUnits(t *testing.T) {
	hiddenWant(t, map[string]time.Duration{"1d12h": 36 * time.Hour, "2d30m": 48*time.Hour + 30*time.Minute})
}

func TestHiddenFractionalDays(t *testing.T) {
	hiddenWant(t, map[string]time.Duration{"1.5d": 36 * time.Hour, "0.5d": 12 * time.Hour})
}

func TestHiddenExistingFormsUnchanged(t *testing.T) {
	hiddenWant(t, map[string]time.Duration{
		"90": 90 * time.Second, "1m30s": 90 * time.Second, "1.5h": 90 * time.Minute, "-1h": -time.Hour,
	})
}

func TestHiddenInvalidValuesKeepTheirMessage(t *testing.T) {
	for _, value := range []string{"2x", "d", "1dd", "soon"} {
		_, err := hiddenDuration(t, value)
		want := `kvconf: s.k: "` + value + `" is not a duration`
		if err == nil || err.Error() != want {
			t.Errorf("GetDuration(%q) error = %v, want %q", value, err, want)
		}
	}
}
