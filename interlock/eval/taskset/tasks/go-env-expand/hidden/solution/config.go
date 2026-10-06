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

// ParseOptions changes how values are read.
type ParseOptions struct {
	// NoExpand keeps ${NAME} references and $$ literally.
	NoExpand bool
}

// Parse reads a configuration from r, expanding environment variables.
func Parse(r io.Reader) (*Config, error) {
	return ParseWithOptions(r, ParseOptions{})
}

// ParseWithOptions reads a configuration from r.
func ParseWithOptions(r io.Reader, opts ParseOptions) (*Config, error) {
	return parse(r, !opts.NoExpand)
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
