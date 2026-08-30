use super::*;
use crate::core::UnitKind;
use crate::ingest::units::MAX_TOKENS;

const SAMPLE: &str = r#"{"type":"session","version":3,"id":"s1","timestamp":"2026-08-26T18:00:00.000Z","cwd":"/tmp/repo"}
{"type":"model_change","id":"m1","parentId":null,"timestamp":"2026-08-26T18:00:00.100Z"}
{"type":"message","id":"u1","parentId":"m1","timestamp":"2026-08-26T18:01:00.000Z","message":{"role":"user","content":[{"type":"text","text":"Investigate why refresh_session loops"}]}}
{"type":"message","id":"a1","parentId":"u1","timestamp":"2026-08-26T18:01:05.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"},{"type":"toolCall","id":"c1","name":"read","arguments":{"path":"src/auth.rs"}}]}}
{"type":"message","id":"r1","parentId":"a1","timestamp":"2026-08-26T18:01:05.100Z","message":{"role":"toolResult","toolCallId":"c1","toolName":"read","content":[{"type":"text","text":"900 lines of file contents that must not become evidence"}]}}
{"type":"message","id":"a2","parentId":"r1","timestamp":"2026-08-26T18:02:00.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Found it. Stale tokens were reused."},{"type":"toolCall","id":"c2","name":"edit","arguments":{"path":"src/auth.rs","newText":"fn refresh() {}"}}]}}
{"type":"message","id":"u2","parentId":"a2","timestamp":"2026-08-26T18:05:00.000Z","message":{"role":"user","content":[{"type":"text","text":"Now run the tests"}]}}
{"type":"message","id":"a3","parentId":"u2","timestamp":"2026-08-26T18:05:02.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"c3","name":"bash","arguments":{"command":"cargo test auth"}}]}}
{"type":"compaction","id":"k1","parentId":"a3","timestamp":"2026-08-26T18:06:00.000Z","summary":"old"}
{"type":"custom","id":"x1","parentId":"k1","timestamp":"2026-08-26T18:06:01.000Z","payload":{"anything":true}}
"#;

fn bash_session() -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}",
        r#"{"type":"session","version":3,"id":"s2","cwd":"/tmp/repo"}"#,
        r#"{"type":"message","id":"u1","message":{"role":"user","content":[{"type":"text","text":"Run the auth checks"}]}}"#,
        r#"{"type":"message","id":"a1","message":{"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"bash","arguments":{"command":"cargo test auth"}}]}}"#,
        r#"{"type":"message","id":"r1","message":{"role":"toolResult","content":[{"type":"toolResult","toolCallId":"c1","toolName":"bash","text":"{\"exitCode\":1,\"durationMs\":4200,\"failed\":2}"}]}}"#,
        r#"{"type":"message","id":"a2","message":{"role":"assistant","content":[{"type":"toolCall","id":"c2","name":"bash","arguments":{"command":"cargo build"}}]}}"#,
    )
}

fn user_turn(id: &str, timestamp: &str, text: &str) -> String {
    format!(
        r#"{{"type":"message","id":"{id}","timestamp":"{timestamp}","message":{{"role":"user","content":[{{"type":"text","text":"{text}"}}]}}}}"#
    )
}

#[test]
fn user_turns_become_single_episode_units() {
    let units = ingest_pi_session(SAMPLE, "s1").unwrap();
    assert_eq!(units.len(), 2, "one unit per user turn");
    for (index, unit) in units.iter().enumerate() {
        assert_eq!(unit.kind, UnitKind::Episode);
        assert_eq!(unit.metadata["episode"], (index + 1) as u64);
        assert_eq!(unit.metadata["session"], "s1");
        assert_eq!(unit.metadata["policy_version"], TURN_POLICY_VERSION);
        assert_eq!(unit.metadata["pieces"], 1);
        assert!(unit.routing_text.contains("source: agent_episode"));
        assert!(unit.evidence_text.starts_with("pi-session:s1 > episode"));
    }
    let first = &units[0];
    assert!(first.evidence_text.contains("User:"));
    assert!(first
        .evidence_text
        .contains("Investigate why refresh_session loops"));
    assert!(first
        .evidence_text
        .contains("Found it. Stale tokens were reused."));
    assert!(first.evidence_text.contains("Tool: read src/auth.rs"));
    assert!(first.metadata["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "src/auth.rs"));
    assert!(first.metadata["timestamp"].is_i64());
    assert!(first.metadata["source_range"]["start_byte"].is_u64());
    let second = &units[1];
    assert!(second.metadata["timestamp"].is_i64());
    assert!(second.metadata["commands"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "cargo test auth"));
}

#[test]
fn tool_outputs_never_become_evidence() {
    let units = ingest_pi_session(SAMPLE, "s1").unwrap();
    for unit in &units {
        assert!(!unit.evidence_text.contains("900 lines of file contents"));
    }
}

#[test]
fn structured_bash_outcomes_are_captured_and_unknowns_stay_unknown() {
    let units = ingest_pi_session(&bash_session(), "s2").unwrap();
    assert_eq!(units.len(), 1);
    let outcomes = units[0].metadata["outcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0]["command"], "cargo test auth");
    assert_eq!(outcomes[0]["outcome"], "failed");
    assert_eq!(outcomes[0]["exit_code"], 1);
    assert_eq!(outcomes[0]["duration_ms"], 4200);
    assert_eq!(outcomes[0]["test_counts"]["failed"], 2);
    assert_eq!(outcomes[1]["outcome"], "unknown");
    assert!(outcomes[1].get("exit_code").is_none());
    assert!(units[0].evidence_text.contains("Command: cargo test auth"));
    assert!(units[0].evidence_text.contains("Outcome: failed"));
}

#[test]
fn pre_first_user_content_is_skipped() {
    let session = format!(
        "{}\n{}\n{}\n{}",
        r#"{"type":"session","version":3,"id":"s3","cwd":"/tmp/repo"}"#,
        r#"{"type":"message","id":"a0","message":{"role":"assistant","content":[{"type":"text","text":"orphaned preamble"},{"type":"toolCall","id":"c0","name":"bash","arguments":{"command":"echo orphan"}}]}}"#,
        r#"{"type":"message","id":"r0","message":{"role":"toolResult","content":[{"type":"toolResult","toolCallId":"c0","toolName":"bash","text":"{\"exitCode\":0}"}]}}"#,
        user_turn("u1", "2026-08-26T18:00:00.000Z", "First real request"),
    );
    let units = ingest_pi_session(&session, "s3").unwrap();
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].metadata["episode"], 1);
    assert!(!units[0].evidence_text.contains("orphaned preamble"));
    assert!(!units[0].evidence_text.contains("echo orphan"));
}

#[test]
fn oversized_turns_split_into_pieces_within_budget() {
    let long = "Walk the auth module top down and explain every step. ".repeat(120);
    let session = format!(
        "{}\n{}\n{}",
        r#"{"type":"session","version":3,"id":"s4","cwd":"/tmp/repo"}"#,
        user_turn("u1", "2026-08-26T18:00:00.000Z", "Explain the module"),
        r#"{"type":"message","id":"a1","message":{"role":"assistant","content":[{"type":"text","text":"Walk the auth module top down and explain every step."},{"type":"toolCall","id":"c1","name":"edit","arguments":{"path":"src/auth.rs"}}]}}"#,
    );
    let session = session.replace(
        "Walk the auth module top down and explain every step.\"",
        &format!("{long}\""),
    );
    let units = ingest_pi_session(&session, "s4").unwrap();
    assert!(units.len() > 1, "long turn must split: {}", units.len());
    for (offset, unit) in units.iter().enumerate() {
        assert_eq!(unit.kind, UnitKind::Episode);
        assert!(unit.token_count <= MAX_TOKENS, "piece {}", offset);
        assert_eq!(unit.metadata["piece"], (offset + 1) as u64);
        assert_eq!(unit.metadata["pieces"], units.len() as u64);
        assert!(unit
            .evidence_text
            .contains(&format!("piece {}", offset + 1)));
        assert!(unit
            .anchors
            .iter()
            .any(|anchor| anchor.kind == crate::core::AnchorKind::Session));
        assert!(unit
            .anchors
            .iter()
            .any(|anchor| anchor.value == "src/auth.rs"));
    }
    let hashes: std::collections::HashSet<_> =
        units.iter().map(|unit| unit.content_hash.clone()).collect();
    assert_eq!(hashes.len(), units.len(), "pieces hash distinctly");
}

#[test]
fn episode_cap_keeps_the_newest_turns() {
    let mut session = String::from(r#"{"type":"session","version":3,"id":"s5","cwd":"/tmp/repo"}"#);
    for index in 0..(MAX_EPISODES_PER_SESSION + 2) {
        session.push('\n');
        session.push_str(&user_turn(
            &format!("u{index}"),
            "2026-08-26T18:00:00.000Z",
            &format!("Request number {index}"),
        ));
    }
    let units = ingest_pi_session(&session, "s5").unwrap();
    assert_eq!(units.len(), MAX_EPISODES_PER_SESSION);
    assert_eq!(units[0].metadata["episode"], 1, "oldest turns dropped, renumbered");
    assert_eq!(
        units.last().unwrap().metadata["episode"],
        MAX_EPISODES_PER_SESSION as u64
    );
}

#[test]
fn identical_sessions_hash_identically_and_sessions_differ() {
    let first = ingest_pi_session(SAMPLE, "s1").unwrap();
    let again = ingest_pi_session(SAMPLE, "s1").unwrap();
    let other = ingest_pi_session(SAMPLE, "other").unwrap();
    for index in 0..first.len() {
        assert_eq!(first[index].content_hash, again[index].content_hash);
        assert_ne!(first[index].content_hash, other[index].content_hash);
    }
}

#[test]
fn timestamps_parse_from_iso8601() {
    assert_eq!(
        jsonl::parse_timestamp("2026-08-26T18:01:05.000Z"),
        Some(1_787_767_265)
    );
    assert_eq!(jsonl::parse_timestamp("not-a-time"), None);
    assert_eq!(jsonl::parse_timestamp("short"), None);
}

#[test]
fn directory_name_matches_pi_convention() {
    assert_eq!(
        session_directory_name("/home/user/Projects/snoop"),
        "--home-user-Projects-snoop--"
    );
}

#[test]
fn locator_keeps_the_pi_session_prefix() {
    assert_eq!(session_locator("abc"), "pi-session:abc");
}
