//! Recognizes check output that shows nothing was checked: a test runner
//! that found no tests exits 0 and would otherwise count as a pass. Each
//! detector is narrow, so a real run is never mistaken for an empty one.

/// The detector that matched, if the output shows the check ran nothing.
pub fn detect(output: &str) -> Option<&'static str> {
    let ran_counts = |prefix: &str, suffix: &str| -> Vec<u64> {
        output
            .lines()
            .filter_map(|l| l.trim().strip_prefix(prefix)?.split_whitespace().next()?.parse().ok())
            .filter(|_| suffix.is_empty() || output.contains(suffix))
            .collect()
    };
    // Python unittest: "Ran 0 tests in 0.000s".
    let unittest = ran_counts("Ran ", " test");
    if !unittest.is_empty() && unittest.iter().all(|n| *n == 0) {
        return Some("unittest ran 0 tests");
    }
    // pytest: "collected 0 items" or "no tests ran".
    if output.contains("collected 0 items") || output.contains("no tests ran") {
        return Some("pytest collected no tests");
    }
    // cargo test: every "running N tests" line says 0.
    let cargo = ran_counts("running ", " test");
    if !cargo.is_empty() && cargo.iter().all(|n| *n == 0) && output.contains("test result:") {
        return Some("cargo test ran 0 tests");
    }
    // go test: only packages without test files.
    if output.contains("[no test files]") && !output.lines().any(|l| l.starts_with("ok ") || l.starts_with("ok\t")) {
        return Some("go test found no test files");
    }
    // Jest and Vitest.
    if output.contains("No tests found") || output.contains("No test files found") {
        return Some("jest or vitest found no tests");
    }
    // Mocha: "0 passing" with nothing failing.
    if output.lines().any(|l| l.trim().starts_with("0 passing")) && !output.contains("failing") {
        return Some("mocha ran 0 tests");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::detect;

    #[test]
    fn empty_runs_are_recognized() {
        assert!(detect("\n----------------------------------------------------------------------\nRan 0 tests in 0.000s\n\nOK\n").is_some());
        assert!(detect("============ no tests ran in 0.01s ============").is_some());
        assert!(detect("collected 0 items\n").is_some());
        assert!(detect("running 0 tests\n\ntest result: ok. 0 passed; 0 failed\n").is_some());
        assert!(detect("?   \texample.com/x\t[no test files]\n").is_some());
        assert!(detect("No tests found, exiting with code 1\n").is_some());
        assert!(detect("\n  0 passing (2ms)\n").is_some());
    }

    #[test]
    fn real_runs_are_not() {
        assert_eq!(detect("..\nRan 2 tests in 0.001s\n\nOK\n"), None);
        assert_eq!(detect("collected 12 items\n\n12 passed in 0.5s"), None);
        assert_eq!(
            detect("running 3 tests\ntest result: ok. 3 passed\nrunning 0 tests\ntest result: ok. 0 passed"),
            None
        );
        assert_eq!(detect("ok  \texample.com/x\t0.01s\n?   \texample.com/y\t[no test files]\n"), None);
        assert_eq!(detect("ok: no duplicate rows after a retry\n"), None);
        assert_eq!(detect(""), None, "a silent check is not an empty test run");
    }
}
