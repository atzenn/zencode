//! Environment-context deltas (Việc 5): send the model a full environment
//! snapshot on the first request of a session and only the fields that
//! changed on later requests, instead of re-sending the whole snapshot
//! every time.
//!
//! Reconciliation with the code as it actually is (main @ 84b8fef): there is
//! no structured per-turn env snapshot upstream. The environment context
//! rides in the system prompt, which `zen-cli` builds once per session
//! (`build_system_prompt`); `zen-agent` prepends that string to every
//! request in [`crate::agent::Agent::request_messages`]. This module is the
//! structured snapshot + delta machinery, hooked at that assembly point:
//! when [`crate::agent::AgentConfig::env_context_deltas`] is on, each
//! request's system message gets one compact env block appended (after the
//! cache-volatile marker, so the stable prefix stays byte-identical):
//! turn 0 carries the full snapshot, later turns carry only changed
//! fields, and every [`RESYNC_INTERVAL`] turns the full snapshot is resent
//! so a lost or diverged delta can never leave the model with stale state.
//!
//! Field set (plan section 2.1, leader Q3): `cwd`, `env`, `git`, `files`,
//! `shell`. New fields only ever enter through the env allowlist plus
//! redaction below; nothing here touches the stream-json contract.

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// Resend the full snapshot every this many turns (leader Q4).
pub const RESYNC_INTERVAL: u64 = 10;

/// Snapshot fields, in canonical order for hashing and payload assembly.
pub const FIELDS: [&str; 5] = ["cwd", "env", "git", "files", "shell"];

/// The only env vars allowed into the snapshot (plan section 2.1). Anything
/// not listed here is never sent, so secrets cannot leak by accident.
pub const ENV_ALLOWLIST: &[&str] = &[
    "PATH", "HOME", "LANG", "LC_ALL", "TERM", "SHELL", "EDITOR", "VISUAL", "USER",
];

/// Replacement text for a value that looks like a secret.
pub const REDACTED: &str = "<redacted>";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitInfo {
    pub branch: Option<String>,
    pub sha: Option<String>,
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShellInfo {
    pub kind: String,
    pub os: String,
    pub arch: String,
}

/// One environment snapshot. `env` is a `BTreeMap` and `files` is sorted,
/// so serialization is deterministic and per-field hashes are stable for
/// identical data (plan section 2.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvSnapshot {
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub git: Option<GitInfo>,
    pub files: Vec<String>,
    pub shell: ShellInfo,
}

/// True when a value looks like a credential: a known token prefix anywhere
/// in it, or `user:password@` userinfo inside a URL. Best-effort defence in
/// depth - the allowlist is the primary control.
fn looks_secret(value: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "sk-",
        "sk_",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "github_pat_",
        "AKIA",
        "xoxb-",
        "xoxp-",
        "-----BEGIN",
    ];
    if PREFIXES.iter().any(|p| value.contains(p)) {
        return true;
    }
    if let Some((_, rest)) = value.split_once("://") {
        if let Some((userinfo, _)) = rest.split_once('@') {
            if userinfo.contains(':') {
                return true;
            }
        }
    }
    false
}

/// Redact a single env value when it looks like a secret.
pub fn redact_value(value: &str) -> String {
    if looks_secret(value) {
        REDACTED.to_string()
    } else {
        value.to_string()
    }
}

/// Run git in `cwd`; `None` on any failure (not a repo, git missing, ...).
fn git_out(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Probe git state: branch + HEAD sha + dirty flag + changed file list.
/// Returns `(None, vec![])` outside a work tree.
fn probe_git(cwd: &Path) -> (Option<GitInfo>, Vec<String>) {
    if git_out(cwd, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        return (None, Vec::new());
    }
    let branch = git_out(cwd, &["symbolic-ref", "--short", "-q", "HEAD"]).filter(|s| !s.is_empty());
    let sha = git_out(cwd, &["rev-parse", "HEAD"]).filter(|s| !s.is_empty());
    let mut files: Vec<String> = Vec::new();
    if let Some(porcelain) = git_out(cwd, &["status", "--porcelain=v1"]) {
        for line in porcelain.lines() {
            if line.len() < 4 {
                continue;
            }
            let mut path = &line[3..];
            if let Some((_, new)) = path.rsplit_once(" -> ") {
                path = new;
            }
            let path = path.trim().trim_matches('"').to_string();
            if !path.is_empty() {
                files.push(path);
            }
        }
    }
    files.sort();
    files.dedup();
    let dirty = !files.is_empty();
    (Some(GitInfo { branch, sha, dirty }), files)
}

impl EnvSnapshot {
    /// Collect a snapshot of the live environment at `cwd`.
    pub fn collect(cwd: &Path) -> EnvSnapshot {
        Self::collect_from(cwd, std::env::vars())
    }

    /// Same as [`EnvSnapshot::collect`] but with an explicit env source, so
    /// tests can prove the allowlist/redaction behaviour deterministically.
    pub fn collect_from<I>(cwd: &Path, vars: I) -> EnvSnapshot
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let mut env: BTreeMap<String, String> = BTreeMap::new();
        let mut shell_path: Option<String> = None;
        for (k, v) in vars {
            if k == "SHELL" {
                shell_path = Some(v.clone());
            }
            if ENV_ALLOWLIST.contains(&k.as_str()) {
                env.insert(k, redact_value(&v));
            }
        }
        let kind = shell_path
            .as_deref()
            .and_then(|s| s.rsplit('/').next())
            .filter(|s| !s.is_empty())
            .unwrap_or("unknown")
            .to_string();
        let (git, files) = probe_git(cwd);
        EnvSnapshot {
            cwd: cwd.display().to_string(),
            env,
            git,
            files,
            shell: ShellInfo {
                kind,
                os: std::env::consts::OS.to_string(),
                arch: std::env::consts::ARCH.to_string(),
            },
        }
    }

    /// Canonical JSON for one field (deterministic key/element order).
    fn field_json(&self, field: &str) -> String {
        match field {
            "cwd" => serde_json::to_string(&self.cwd),
            "env" => serde_json::to_string(&self.env),
            "git" => serde_json::to_string(&self.git),
            "files" => serde_json::to_string(&self.files),
            "shell" => serde_json::to_string(&self.shell),
            _ => Ok("null".to_string()),
        }
        .unwrap_or_else(|_| "null".to_string())
    }

    /// First 16 hex chars of the field's SHA-256 (plan section 2.2).
    pub fn field_hash(&self, field: &str) -> String {
        sha256_hex(self.field_json(field).as_bytes())[..16].to_string()
    }

    fn field_value(&self, field: &str) -> serde_json::Value {
        serde_json::from_str(&self.field_json(field)).unwrap_or(serde_json::Value::Null)
    }
}

/// One turn's env payload (internal format, plan section 2.4 - NOT the
/// stream-json contract). Full: `{"partial": false, "snapshot": {...}}`.
/// Delta: `{"partial": true, "base_turn": N, "snapshot": {changed only}}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EnvDelta {
    pub partial: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_turn: Option<u64>,
    pub snapshot: serde_json::Value,
    /// Turn this payload belongs to (not serialized; for rendering/tests).
    #[serde(skip)]
    pub turn: u64,
}

/// Per-session delta state: the field hashes of the last snapshot sent and
/// the turn counters that drive the resync cadence.
#[derive(Debug, Default)]
pub struct DeltaStore {
    last_hashes: BTreeMap<&'static str, String>,
    last_full_turn: Option<u64>,
    next_turn: u64,
}

impl DeltaStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget all state (session resume/reset): the next diff is a full
    /// snapshot again (plan section 2.2).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Diff `snap` against the last snapshot sent and advance the turn
    /// counter. Full on the first call and every [`RESYNC_INTERVAL`] turns;
    /// otherwise only fields whose hash changed are included.
    pub fn diff(&mut self, snap: &EnvSnapshot) -> EnvDelta {
        let turn = self.next_turn;
        let full = match self.last_full_turn {
            None => true,
            Some(t) => turn.saturating_sub(t) >= RESYNC_INTERVAL,
        };
        let mut obj = serde_json::Map::new();
        let mut hashes: BTreeMap<&'static str, String> = BTreeMap::new();
        for field in FIELDS {
            let h = snap.field_hash(field);
            let changed = self.last_hashes.get(field) != Some(&h);
            hashes.insert(field, h);
            if full || changed {
                obj.insert(field.to_string(), snap.field_value(field));
            }
        }
        self.last_hashes = hashes;
        if full {
            self.last_full_turn = Some(turn);
        }
        self.next_turn += 1;
        EnvDelta {
            partial: !full,
            base_turn: if full { None } else { Some(turn - 1) },
            snapshot: serde_json::Value::Object(obj),
            turn,
        }
    }
}

/// Render a payload as the text block appended to the system message.
pub fn render_delta(delta: &EnvDelta) -> String {
    let json = serde_json::to_string(delta).unwrap_or_default();
    if delta.partial {
        format!(
            "\n\nEnv context (turn {}, delta since turn {}):\n{}",
            delta.turn,
            delta.base_turn.unwrap_or(0),
            json
        )
    } else {
        format!(
            "\n\nEnv context (turn {}, full snapshot):\n{}",
            delta.turn, json
        )
    }
}

/// Minimal SHA-256 (FIPS 180-4), dependency-free - same approach as
/// `zen_cli::skills::sha256_hex`. Test-vector verified below.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    // msg is padded to a multiple of 64 above, so the remainder is empty.
    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> EnvSnapshot {
        EnvSnapshot {
            cwd: "/repo".to_string(),
            env: BTreeMap::from([
                ("PATH".to_string(), "/usr/bin:/bin".to_string()),
                ("SHELL".to_string(), "/bin/bash".to_string()),
            ]),
            git: Some(GitInfo {
                branch: Some("main".to_string()),
                sha: Some("abc123".to_string()),
                dirty: false,
            }),
            files: vec!["src/lib.rs".to_string()],
            shell: ShellInfo {
                kind: "bash".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            },
        }
    }

    #[test]
    fn sha256_test_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn field_hash_stable_for_same_data_and_key_order() {
        let a = snap();
        let mut b = snap();
        // Same content, different insertion order: hashes must not move.
        b.env = BTreeMap::from([
            ("SHELL".to_string(), "/bin/bash".to_string()),
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ]);
        for f in FIELDS {
            assert_eq!(a.field_hash(f), b.field_hash(f), "field {f}");
            assert_eq!(a.field_hash(f).len(), 16);
        }
        let mut c = snap();
        c.git.as_mut().unwrap().dirty = true;
        assert_ne!(a.field_hash("git"), c.field_hash("git"));
        assert_eq!(a.field_hash("cwd"), c.field_hash("cwd"));
    }

    #[test]
    fn turn0_is_full_with_all_fields() {
        let mut store = DeltaStore::new();
        let d = store.diff(&snap());
        assert!(!d.partial);
        assert_eq!(d.base_turn, None);
        assert_eq!(d.turn, 0);
        let keys: Vec<&str> = d
            .snapshot
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        for f in FIELDS {
            assert!(keys.contains(&f), "missing field {f}");
        }
    }

    #[test]
    fn later_turn_sends_only_changed_fields() {
        let mut store = DeltaStore::new();
        store.diff(&snap());
        let mut s2 = snap();
        s2.git = Some(GitInfo {
            branch: Some("main".to_string()),
            sha: Some("def456".to_string()),
            dirty: true,
        });
        let d = store.diff(&s2);
        assert!(d.partial);
        assert_eq!(d.base_turn, Some(0));
        let keys: Vec<&str> = d
            .snapshot
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        assert_eq!(keys, vec!["git"]);
        // Rendered form matches the plan's delta shape.
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("\"partial\":true"), "{json}");
        assert!(json.contains("\"base_turn\":0"), "{json}");
    }

    #[test]
    fn unchanged_turn_sends_empty_delta() {
        let mut store = DeltaStore::new();
        store.diff(&snap());
        let d = store.diff(&snap());
        assert!(d.partial);
        assert!(d.snapshot.as_object().unwrap().is_empty());
    }

    #[test]
    fn resync_full_every_10_turns() {
        let mut store = DeltaStore::new();
        let mut full_turns = Vec::new();
        for _ in 0..21 {
            let d = store.diff(&snap());
            if !d.partial {
                full_turns.push(d.turn);
            }
        }
        assert_eq!(full_turns, vec![0, 10, 20]);
    }

    #[test]
    fn reset_makes_next_diff_full() {
        let mut store = DeltaStore::new();
        store.diff(&snap());
        store.diff(&snap());
        store.reset();
        let d = store.diff(&snap());
        assert!(!d.partial);
        assert_eq!(d.turn, 0);
    }

    #[test]
    fn collect_applies_allowlist_and_redaction() {
        let dir = std::env::temp_dir().join(format!("zen-envctx-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let vars = vec![
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            (
                "AWS_SECRET_ACCESS_KEY".to_string(),
                "wJalrXUtnFEMI".to_string(),
            ),
            (
                "OPENAI_API_KEY".to_string(),
                "sk-should-never-appear".to_string(),
            ),
            (
                "EDITOR".to_string(),
                "https://user:hunter2@example.com".to_string(),
            ),
            ("VISUAL".to_string(), "ghp_0123456789abcdef".to_string()),
            ("SHELL".to_string(), "/bin/zsh".to_string()),
        ];
        let s = EnvSnapshot::collect_from(&dir, vars);
        assert_eq!(s.env.get("PATH").map(String::as_str), Some("/usr/bin:/bin"));
        assert!(!s.env.contains_key("AWS_SECRET_ACCESS_KEY"));
        assert!(!s.env.contains_key("OPENAI_API_KEY"));
        assert_eq!(s.env.get("EDITOR").map(String::as_str), Some(REDACTED));
        assert_eq!(s.env.get("VISUAL").map(String::as_str), Some(REDACTED));
        assert_eq!(s.shell.kind, "zsh");
        // A plain value passes through untouched.
        assert_eq!(redact_value("/usr/bin:/bin"), "/usr/bin:/bin");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_probe_reads_branch_dirty_and_files() {
        let dir = std::env::temp_dir().join(format!("zen-envctx-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .unwrap()
        };
        if !git(&["init", "-b", "main"]).status.success() {
            // git unavailable or too old for -b: nothing meaningful to test.
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        std::fs::write(dir.join("new_file.rs"), "fn x() {}\n").unwrap();
        let s = EnvSnapshot::collect_from(&dir, Vec::<(String, String)>::new());
        let g = s.git.expect("inside a work tree");
        assert_eq!(g.branch.as_deref(), Some("main"));
        assert!(g.dirty);
        assert_eq!(s.files, vec!["new_file.rs".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn agent_appends_full_then_delta_when_enabled() {
        use crate::testutil::{base_cfg, build_agent, resp, FakePerms, MockLlm};
        use zen_core::{PermissionDecision, ToolRegistry};
        let llm = MockLlm::new(vec![resp("one"), resp("two")]);
        let mut cfg = base_cfg();
        cfg.env_context_deltas = true;
        let (mut agent, _sink, _h) = build_agent(llm.clone(), ToolRegistry::new(), FakePerms::new(PermissionDecision::Allow), cfg);
        let r1 = agent.run_turn("hi").await;
        assert!(!r1.is_error);
        let r2 = agent.run_turn("again").await;
        assert!(!r2.is_error);
        let reqs = llm.requests();
        assert_eq!(reqs.len(), 2);
        let sys1 = reqs[0].messages[0].content.clone().unwrap();
        assert!(
            sys1.contains("Env context (turn 0, full snapshot)"),
            "{sys1}"
        );
        assert!(sys1.contains("\"partial\":false"), "{sys1}");
        let sys2 = reqs[1].messages[0].content.clone().unwrap();
        assert!(
            sys2.contains("Env context (turn 1, delta since turn 0)"),
            "{sys2}"
        );
        assert!(sys2.contains("\"partial\":true"), "{sys2}");
    }
}
