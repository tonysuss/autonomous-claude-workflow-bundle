Retention periods in our configuration files are written in days, but `GetDuration` does not understand them: `retention = 30d` fails with `kvconf: store.retention: "30d" is not a duration`.

Make `GetDuration` accept the unit `d`, for days of 24 hours, so that `30d` is 720 hours. Everything `GetDuration` accepts today must keep working as it does now.

`checks/days.sh` checks the reported value. The tests run with `go test ./...`.
