// Package kvconf reads simple INI-style configuration files:
//
//	# a comment
//	name = demo
//
//	[server]
//	host = example.org
//	port = 8080
//	title = "My App"   # an inline comment
//
// Keys before the first section header belong to the unnamed section "".
package kvconf

import (
	"io"
	"os"
	"sort"
	"strings"
)

// Config holds parsed values by section and key.
type Config struct {
	sections map[string]map[string]string
	order    []string // section names in first-seen order
}

func newConfig() *Config {
	return &Config{sections: map[string]map[string]string{"": {}}, order: []string{""}}
}

func (c *Config) addSection(name string) {
	if _, ok := c.sections[name]; !ok {
		c.sections[name] = map[string]string{}
		c.order = append(c.order, name)
	}
}

func (c *Config) set(section, key, value string) {
	c.addSection(section)
	c.sections[section][key] = value
}

// Sections returns the section names in the order they first appear. The
// unnamed section "" always comes first.
func (c *Config) Sections() []string {
	return append([]string(nil), c.order...)
}

// Keys returns the keys of a section in sorted order.
func (c *Config) Keys(section string) []string {
	keys := make([]string, 0, len(c.sections[section]))
	for k := range c.sections[section] {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}

// Lookup returns a value by the name the command line uses, SECTION.KEY.
// A name without a dot is a key in the unnamed section.
func (c *Config) Lookup(name string) (string, error) {
	i := strings.LastIndex(name, ".")
	if i < 0 {
		return c.Get("", name)
	}
	return c.Get(name[:i], name[i+1:])
}

// Parse reads a configuration from r.
func Parse(r io.Reader) (*Config, error) {
	return parse(r)
}

// Load parses the file at path.
func Load(path string) (*Config, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	return Parse(f)
}
