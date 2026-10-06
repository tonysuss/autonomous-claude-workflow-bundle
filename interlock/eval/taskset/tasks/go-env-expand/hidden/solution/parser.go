package kvconf

import (
	"bufio"
	"fmt"
	"io"
	"os"
	"strings"
)

func parse(r io.Reader, expandVars bool) (*Config, error) {
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
		if err == nil && expandVars {
			value, err = expand(value)
		}
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

// parseValue handles quoting and inline comments. An unquoted value ends at
// the first '#'. A double-quoted value may contain '#', and \" and \\ stand
// for a quote and a backslash; after the closing quote only a comment may
// follow.
func parseValue(raw string) (string, error) {
	if !strings.HasPrefix(raw, `"`) {
		if i := strings.Index(raw, "#"); i >= 0 {
			raw = raw[:i]
		}
		return strings.TrimSpace(raw), nil
	}
	var b strings.Builder
	for i := 1; i < len(raw); i++ {
		ch := raw[i]
		switch {
		case ch == '\\' && i+1 < len(raw) && (raw[i+1] == '"' || raw[i+1] == '\\'):
			b.WriteByte(raw[i+1])
			i++
		case ch == '"':
			rest := strings.TrimSpace(raw[i+1:])
			if rest != "" && !strings.HasPrefix(rest, "#") {
				return "", fmt.Errorf("unexpected text after closing quote: %q", rest)
			}
			return b.String(), nil
		default:
			b.WriteByte(ch)
		}
	}
	return "", fmt.Errorf("unterminated quoted value")
}

// expand replaces ${NAME} and ${NAME:-fallback} with environment values and
// $$ with $. Any other $ is kept.
func expand(s string) (string, error) {
	var b strings.Builder
	for i := 0; i < len(s); i++ {
		if s[i] != '$' || i+1 >= len(s) {
			b.WriteByte(s[i])
			continue
		}
		switch s[i+1] {
		case '$':
			b.WriteByte('$')
			i++
		case '{':
			end := strings.IndexByte(s[i+2:], '}')
			if end < 0 {
				return "", fmt.Errorf("unterminated variable reference")
			}
			name, fallback, hasFallback := strings.Cut(s[i+2:i+2+end], ":-")
			v, ok := os.LookupEnv(name)
			switch {
			case ok && v != "":
				b.WriteString(v)
			case hasFallback:
				b.WriteString(fallback)
			case !ok:
				return "", fmt.Errorf("undefined variable %q", name)
			}
			i += 2 + end
		default:
			b.WriteByte('$')
		}
	}
	return b.String(), nil
}
