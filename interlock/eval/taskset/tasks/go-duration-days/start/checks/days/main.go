// Command days prints store.retention from checks/days.conf as a duration.
package main

import (
	"fmt"
	"os"

	"example.com/kvconf"
)

func main() {
	cfg, err := kvconf.Load("checks/days.conf")
	if err == nil {
		var d interface{ String() string }
		d, err = cfg.GetDuration("store", "retention")
		if err == nil {
			fmt.Println(d)
			return
		}
	}
	fmt.Fprintln(os.Stderr, err)
	os.Exit(1)
}
