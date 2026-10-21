#![cfg(unix)]

use git_monitor::process::{run, run_with_input, Shutdown};
use std::{
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn deadline_covers_inherited_stdout_and_stderr_after_parent_exit() {
    for script in ["sleep 3 & exit 0", "sleep 3 >&2 & exit 0"] {
        let start = Instant::now();
        let error = run(
            Command::new("sh").args(["-c", script]),
            Duration::from_millis(150),
            1024,
            &Shutdown::default(),
            Duration::from_secs(1),
            None,
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("process timeout"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}

#[test]
fn deadline_cancels_blocked_stdin_writer_after_parent_exit() {
    let start = Instant::now();
    // Explicit stdin redirection stops the shell from assigning /dev/null to
    // its background job, leaving the monitor's writer blocked on a full pipe.
    let error = run_with_input(
        Command::new("sh").args(["-c", "sleep 3 <&0 & exit 0"]),
        vec![b'x'; 1_000_000],
        Duration::from_millis(150),
        1024,
        &Shutdown::default(),
        Duration::from_secs(1),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("process timeout"));
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn successful_process_drains_exact_output_and_input() {
    let input = vec![b'x'; 100_000];
    let result = run_with_input(
        Command::new("sh").args(["-c", "cat; printf diagnostic >&2"]),
        input.clone(),
        Duration::from_secs(3),
        input.len(),
        &Shutdown::default(),
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(result.code, 0);
    assert_eq!(result.stdout, input);
    assert_eq!(result.stderr, b"diagnostic");
}

#[test]
fn output_budget_and_unsuccessful_exit_are_reported() {
    let error = run(
        Command::new("sh").args(["-c", "printf 'too much output'"]),
        Duration::from_secs(3),
        2,
        &Shutdown::default(),
        Duration::from_secs(1),
        None,
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("output budget exceeded"));
    let result = run(
        Command::new("sh").args(["-c", "printf failure >&2; exit 7"]),
        Duration::from_secs(3),
        1024,
        &Shutdown::default(),
        Duration::from_secs(1),
        None,
    )
    .unwrap();
    assert_eq!(result.code, 7);
    assert_eq!(result.stderr, b"failure");
}
