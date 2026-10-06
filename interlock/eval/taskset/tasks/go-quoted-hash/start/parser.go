package kvconf

import (
	"bufio"
	"fmt"
	"io"
	"strings"
)

func parse(r io.Reader) (*Config, error) {
	c := newConfig()
	section := ""
	sc := bufio.NewScanner(r)
	n := 0
	for sc.Scan() {
		n++
		line := strings.TrimSpace(sc.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		if strings.HasPrefix(line, "[") {
			if !strings.HasSuffix(line, "]") {
				return nil, fmt.Errorf("kvconf: line %d: unterminated section header", n)
			}
			section = strings.TrimSpace(line[1 : len(line)-1])
			if section == "" {
				return nil, fmt.Errorf("kvconf: line %d: empty section name", n)
			}
			c.addSection(section)
			continue
		}
		key, raw, ok := strings.Cut(line, "=")
		if !ok {
			return nil, fmt.Errorf("kvconf: line %d: expected key = value", n)
		}
		key = strings.TrimSpace(key)
		if key == "" {
			return nil, fmt.Errorf("kvconf: line %d: missing key", n)
		}
		value, err := parseValue(strings.TrimSpace(raw))
		if err != nil {
			return nil, fmt.Errorf("kvconf: line %d: %v", n, err)
		}
		c.set(section, key, value)
	}
	if err := sc.Err(); err != nil {
		return nil, err
	}
	return c, nil
}

// parseValue strips an inline comment, then removes surrounding quotes.
func parseValue(raw string) (string, error) {
	if i := strings.Index(raw, "#"); i >= 0 {
		raw = strings.TrimSpace(raw[:i])
	}
	if strings.HasPrefix(raw, `"`) {
		if len(raw) < 2 || !strings.HasSuffix(raw, `"`) {
			return "", fmt.Errorf("unterminated quoted value")
		}
		return strings.ReplaceAll(raw[1:len(raw)-1], `\"`, `"`), nil
	}
	return raw, nil
}
