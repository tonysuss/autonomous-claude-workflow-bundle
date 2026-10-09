package kvconf

import (
	"fmt"
	"regexp"
	"strconv"
	"strings"
	"time"
)

// Get returns the value of key in section.
func (c *Config) Get(section, key string) (string, error) {
	sec, ok := c.sections[section]
	if !ok {
		return "", fmt.Errorf("kvconf: no section %q", section)
	}
	v, ok := sec[key]
	if !ok {
		return "", fmt.Errorf("kvconf: %s: no key %q", sectionLabel(section), key)
	}
	return v, nil
}

// GetInt returns the value of key in section as an int. Underscores may
// separate digits, as in 1_000.
func (c *Config) GetInt(section, key string) (int, error) {
	sec, ok := c.sections[section]
	if !ok {
		return 0, fmt.Errorf("kvconf: no section %q", section)
	}
	v, ok := sec[key]
	if !ok {
		return 0, fmt.Errorf("kvconf: %s: no key %q", sectionLabel(section), key)
	}
	n, err := strconv.Atoi(strings.ReplaceAll(v, "_", ""))
	if err != nil {
		return 0, fmt.Errorf("kvconf: %s.%s: %q is not an integer", section, key, v)
	}
	return n, nil
}

// GetBool returns the value of key in section as a bool. It accepts true,
// yes, on and 1, and false, no, off and 0, in any case.
func (c *Config) GetBool(section, key string) (bool, error) {
	sec, ok := c.sections[section]
	if !ok {
		return false, fmt.Errorf("kvconf: no section %q", section)
	}
	v, ok := sec[key]
	if !ok {
		return false, fmt.Errorf("kvconf: %s: key %q not set", sectionLabel(section), key)
	}
	switch strings.ToLower(v) {
	case "true", "yes", "on", "1":
		return true, nil
	case "false", "no", "off", "0":
		return false, nil
	}
	return false, fmt.Errorf("kvconf: %s.%s: %q is not a boolean", section, key, v)
}

// GetDuration returns the value of key in section as a duration. It accepts
// Go duration syntax ("1m30s") or a whole number of seconds ("90").
func (c *Config) GetDuration(section, key string) (time.Duration, error) {
	sec, ok := c.sections[section]
	if !ok {
		return 0, fmt.Errorf("kvconf: no section %q", section)
	}
	v, ok := sec[key]
	if !ok {
		return 0, fmt.Errorf("kvconf: %s: no key %q", sectionLabel(section), key)
	}
	if n, err := strconv.Atoi(v); err == nil {
		return time.Duration(n) * time.Second, nil
	}
	d, err := time.ParseDuration(daysAsHours(v))
	if err != nil {
		return 0, fmt.Errorf("kvconf: %s.%s: %q is not a duration", section, key, v)
	}
	return d, nil
}

func sectionLabel(section string) string {
	if section == "" {
		return "top level"
	}
	return "[" + section + "]"
}

var dayUnit = regexp.MustCompile(`(\d+(?:\.\d+)?)d`)

// daysAsHours rewrites each "<number>d" in a duration as hours, so "1.5d12h"
// becomes "36h12h", which time.ParseDuration reads.
func daysAsHours(v string) string {
	return dayUnit.ReplaceAllStringFunc(v, func(m string) string {
		f, err := strconv.ParseFloat(m[:len(m)-1], 64)
		if err != nil {
			return m
		}
		return strconv.FormatFloat(f*24, 'f', -1, 64) + "h"
	})
}
