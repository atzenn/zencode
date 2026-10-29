//! Benchmark for Viec 5 env-context deltas (plan section 2.5, leader Q2/Q5).
//!
//! Scenario: a fixed 20-turn agent loop on one sample task - prompt:
//! "Fix the failing parser test in the repo, then run the test suite" -
//! simulated as a scripted sequence of environment states: the agent edits
//! 1-3 files per turn, commits at turns 6, 12 and 18 (git sha changes,
//! dirty flag clears), while cwd / env / shell stay constant, exactly like
//! a real edit-build-test session.
//!
//! Two modes over the same 20 turns:
//! - OFF: the full snapshot is rendered on every turn (fresh DeltaStore).
//! - ON: one DeltaStore for the whole session (full on turn 0 and at the
//!   10-turn resync, deltas otherwise).
//!
//! Token counting: provider `usage` is not available in this environment
//! (no provider credential on the bench machine), so tokens are estimated
//! with the documented chars/4 rule of thumb and the estimate is labelled
//! as such. Only the env-context payload is counted - it is the part this
//! mechanism controls.
//!
//! Run: `cargo run -p zen-agent --example env_delta_bench`

use std::collections::BTreeMap;
use zen_agent::env_context::{render_delta, DeltaStore, EnvSnapshot, GitInfo, ShellInfo};

fn est_tokens(rendered: &str) -> u64 {
    (rendered.chars().count() as f64 / 4.0).ceil() as u64
}

fn scenario_turn(turn: u64) -> EnvSnapshot {
    // Commits land at turns 6, 12, 18: the sha advances and the tree is
    // clean right after; every other turn has work in flight.
    let commits = (turn >= 6) as usize + (turn >= 12) as usize + (turn >= 18) as usize;
    let sha = format!("{:040x}", 0x84b8fef_u64 + commits as u64 * 0x1111);
    let dirty = commits == 0 || !turn.is_multiple_of(6);
    let mut files: Vec<String> = Vec::new();
    if dirty {
        files.push("crates/zen-agent/src/parser.rs".to_string());
        if turn.is_multiple_of(2) {
            files.push("crates/zen-agent/src/lib.rs".to_string());
        }
        if turn % 5 == 3 {
            files.push("crates/zen-agent/tests/parser_tests.rs".to_string());
        }
    }
    EnvSnapshot {
        cwd: "/data/workspace/zencode-zcdev".to_string(),
        env: BTreeMap::from([
            (
                "PATH".to_string(),
                "/home/user/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/usr/local/games".to_string(),
            ),
            ("HOME".to_string(), "/home/user".to_string()),
            ("LANG".to_string(), "en_US.UTF-8".to_string()),
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("SHELL".to_string(), "/bin/bash".to_string()),
            ("EDITOR".to_string(), "vim".to_string()),
        ]),
        git: Some(GitInfo {
            branch: Some("dev/env-context-deltas".to_string()),
            sha: Some(sha),
            dirty,
        }),
        files,
        shell: ShellInfo {
            kind: "bash".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        },
    }
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[idx.saturating_sub(1).min(sorted.len() - 1)]
}

fn main() {
    let turns: Vec<EnvSnapshot> = (0..20).map(scenario_turn).collect();

    let mut off_per_turn: Vec<u64> = Vec::new();
    for snap in &turns {
        let mut fresh = DeltaStore::new();
        let rendered = render_delta(&fresh.diff(snap));
        off_per_turn.push(est_tokens(&rendered));
    }

    let mut store = DeltaStore::new();
    let mut on_per_turn: Vec<u64> = Vec::new();
    for snap in &turns {
        let rendered = render_delta(&store.diff(snap));
        on_per_turn.push(est_tokens(&rendered));
    }

    let off_total: u64 = off_per_turn.iter().sum();
    let on_total: u64 = on_per_turn.iter().sum();
    let reduction = if off_total > 0 {
        100.0 * (1.0 - on_total as f64 / off_total as f64)
    } else {
        0.0
    };

    let mut off_sorted = off_per_turn.clone();
    off_sorted.sort_unstable();
    let mut on_sorted = on_per_turn.clone();
    on_sorted.sort_unstable();

    println!("Env-context delta benchmark - 20-turn scenario (estimated tokens, chars/4)");
    println!(" turn | off (full each turn) | on (delta)");
    for i in 0..20 {
        println!(
            " {:>4} | {:>20} | {:>10}",
            i, off_per_turn[i], on_per_turn[i]
        );
    }
    println!(
        " total input tokens (env payload): OFF {} -> ON {} ({:.1}% reduction)",
        off_total, on_total, reduction
    );
    println!(
        " per-turn p50/p90: OFF {}/{} -> ON {}/{}",
        percentile(&off_sorted, 50.0),
        percentile(&off_sorted, 90.0),
        percentile(&on_sorted, 50.0),
        percentile(&on_sorted, 90.0)
    );
}
