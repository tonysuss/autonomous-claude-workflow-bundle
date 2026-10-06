// Command kvconf prints values from a kvconf file.
//
//	kvconf get [--no-expand] FILE SECTION.KEY   print one value (use KEY alone for the unnamed section)
//	kvconf dump [--no-expand] FILE              print every value as SECTION.KEY=VALUE
package main

import (
	"fmt"
	"io"
	"os"
	"strings"

	"example.com/kvconf"
)

func main() {
	os.Exit(run(os.Args[1:], os.Stdout, os.Stderr))
}

const usage = "usage: kvconf get [--no-expand] FILE SECTION.KEY | kvconf dump [--no-expand] FILE"

func load(path string, noExpand bool) (*kvconf.Config, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	return kvconf.ParseWithOptions(f, kvconf.ParseOptions{NoExpand: noExpand})
}

func run(args []string, stdout, stderr io.Writer) int {
	if len(args) == 0 {
		fmt.Fprintln(stderr, usage)
		return 2
	}
	cmd, rest := args[0], args[1:]
	noExpand := len(rest) > 0 && rest[0] == "--no-expand"
	if noExpand {
		rest = rest[1:]
	}
	switch cmd {
	case "get":
		if len(rest) != 2 {
			fmt.Fprintln(stderr, usage)
			return 2
		}
		cfg, err := load(rest[0], noExpand)
		if err != nil {
			fmt.Fprintln(stderr, err)
			return 1
		}
		section, key := splitName(rest[1])
		v, err := cfg.Get(section, key)
		if err != nil {
			fmt.Fprintln(stderr, err)
			return 1
		}
		fmt.Fprintln(stdout, v)
		return 0
	case "dump":
		if len(rest) != 1 {
			fmt.Fprintln(stderr, usage)
			return 2
		}
		cfg, err := load(rest[0], noExpand)
		if err != nil {
			fmt.Fprintln(stderr, err)
			return 1
		}
		for _, s := range cfg.Sections() {
			for _, k := range cfg.Keys(s) {
				v, _ := cfg.Get(s, k)
				name := k
				if s != "" {
					name = s + "." + k
				}
				fmt.Fprintf(stdout, "%s=%s\n", name, v)
			}
		}
		return 0
	}
	fmt.Fprintln(stderr, usage)
	return 2
}

// splitName splits SECTION.KEY at the last dot; a name without a dot is a
// key in the unnamed section.
func splitName(name string) (string, string) {
	i := strings.LastIndex(name, ".")
	if i < 0 {
		return "", name
	}
	return name[:i], name[i+1:]
}
