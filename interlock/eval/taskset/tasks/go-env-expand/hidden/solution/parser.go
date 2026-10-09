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

// parseValue strips an inline comment, then removes surrounding quotes.
// Inside quotes, \" and \\ stand for a quote and a backslash.
func parseValue(raw string) (string, error) {
	if i := strings.Index(raw, "#"); i >= 0 {
		raw = strings.TrimSpace(raw[:i])
	}
	if strings.HasPrefix(raw, `"`) {
		if len(raw) < 2 || !strings.HasSuffix(raw, `"`) {
			return "", fmt.Errorf("unterminated quoted value")
		}
		return unescape.Replace(raw[1 : len(raw)-1]), nil
	}
	return raw, nil
}

var unescape = strings.NewReplacer(`\\`, `\`, `\"`, `"`)

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
