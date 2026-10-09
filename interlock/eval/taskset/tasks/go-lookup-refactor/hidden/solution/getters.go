package kvconf

import (
	"errors"
	"fmt"
	"strconv"
	"strings"
	"time"
)

// missingKey is lookup's error for a key that is not in its section.
type missingKey struct{ section, key string }

func (e *missingKey) Error() string {
	return fmt.Sprintf("kvconf: %s: no key %q", sectionLabel(e.section), e.key)
}

// lookup returns the raw value of key in section.
func (c *Config) lookup(section, key string) (string, error) {
	sec, ok := c.sections[section]
	if !ok {
		return "", fmt.Errorf("kvconf: no section %q", section)
	}
	v, ok := sec[key]
	if !ok {
		return "", &missingKey{section, key}
	}
	return v, nil
}

// Get returns the value of key in section.
func (c *Config) Get(section, key string) (string, error) {
	return c.lookup(section, key)
}

// GetInt returns the value of key in section as an int. Underscores may
// separate digits, as in 1_000.
func (c *Config) GetInt(section, key string) (int, error) {
	v, err := c.lookup(section, key)
	if err != nil {
		return 0, err
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
	v, err := c.lookup(section, key)
	var missing *missingKey
	if errors.As(err, &missing) {
		return false, fmt.Errorf("kvconf: %s: key %q not set", sectionLabel(section), key)
	}
	if err != nil {
		return false, err
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
	v, err := c.lookup(section, key)
	if err != nil {
		return 0, err
	}
	if n, err := strconv.Atoi(v); err == nil {
		return time.Duration(n) * time.Second, nil
	}
	d, err := time.ParseDuration(v)
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
