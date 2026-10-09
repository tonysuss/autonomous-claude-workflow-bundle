// Command kvconf prints values from a kvconf file.
//
//	kvconf get FILE SECTION.KEY   print one value (use KEY alone for the unnamed section)
//	kvconf dump FILE              print every value as SECTION.KEY=VALUE
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

const usage = "usage: kvconf get FILE SECTION.KEY | kvconf dump FILE"

func run(args []string, stdout, stderr io.Writer) int {
	if len(args) == 0 {
		fmt.Fprintln(stderr, usage)
		return 2
	}
	switch args[0] {
	case "get":
		if len(args) != 3 {
			fmt.Fprintln(stderr, usage)
			return 2
		}
		cfg, err := kvconf.Load(args[1])
		if err != nil {
			fmt.Fprintln(stderr, err)
			return 1
		}
		section, key := splitName(args[2])
		v, err := cfg.Get(section, key)
		if err != nil {
			fmt.Fprintln(stderr, err)
			return 1
		}
		fmt.Fprintln(stdout, v)
		return 0
	case "dump":
		if len(args) != 2 {
			fmt.Fprintln(stderr, usage)
			return 2
		}
		cfg, err := kvconf.Load(args[1])
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

// splitName splits SECTION.KEY into its parts; a name without a dot is a key
// in the unnamed section.
func splitName(name string) (string, string) {
	i := strings.LastIndex(name, ".")
	if i < 0 {
		return "", name
	}
	return name[:i], name[i+1:]
}
