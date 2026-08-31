use snoop::core::UnitKind;
use snoop::ingest::harness::ingest_pi_session;
use snoop::ingest::units::MAX_TOKENS;

const LONG_SENTENCE: &str = "Walk the auth module top down, quote every relevant span, and explain the reasoning behind each step in enough detail for a later review. ";

fn long_text() -> String {
    LONG_SENTENCE.repeat(8)
}

fn multi_cycle_session() -> String {
    let long = long_text();
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n",
        r#"{"type":"session","version":3,"id":"seg-e2e","cwd":"/tmp/repo"}"#,
        line_user(
            "u1",
            "Please investigate the failing refresh_session test and fix the root cause."
        ),
        line_text_plus_call("a1", &long, "c1", "read", r#""path":"src/auth.rs""#),
        line_result("r1", "c1", "read", "three matching call sites"),
        line_text_plus_call("a2", &long, "c2", "edit", r#""path":"src/auth.rs""#),
        line_result("r2", "c2", "edit", "edit applied"),
        line_call("a3", "c3", "bash", r#""command":"cargo test""#),
        line_result("r3", "c3", "bash", r#"{\"exitCode\":1}"#),
        line_text_plus_call("a4", &long, "c4", "edit", r#""path":"src/auth.rs""#),
        line_result("r4", "c4", "edit", "edit applied"),
        line_call("a5", "c5", "bash", r#""command":"cargo test""#),
        line_result("r5", "c5", "bash", r#"{\"exitCode\":0}"#),
        line_text("a6", &long),
    )
}

fn line_user(id: &str, text: &str) -> String {
    format!(
        r#"{{"type":"message","id":"{id}","message":{{"role":"user","content":[{{"type":"text","text":"{text}"}}]}}}}"#
    )
}

fn line_text(id: &str, text: &str) -> String {
    format!(
        r#"{{"type":"message","id":"{id}","message":{{"role":"assistant","content":[{{"type":"text","text":"{text}"}}]}}}}"#
    )
}

fn line_call(id: &str, call_id: &str, tool: &str, arguments: &str) -> String {
    format!(
        r#"{{"type":"message","id":"{id}","message":{{"role":"assistant","content":[{{"type":"toolCall","id":"{call_id}","name":"{tool}","arguments":{{{arguments}}}}}]}}}}"#
    )
}

fn line_text_plus_call(id: &str, text: &str, call_id: &str, tool: &str, arguments: &str) -> String {
    format!(
        r#"{{"type":"message","id":"{id}","message":{{"role":"assistant","content":[{{"type":"text","text":"{text}"}},{{"type":"toolCall","id":"{call_id}","name":"{tool}","arguments":{{{arguments}}}}}]}}}}"#
    )
}

fn line_result(id: &str, call_id: &str, tool: &str, text: &str) -> String {
    format!(
        r#"{{"type":"message","id":"{id}","message":{{"role":"toolResult","content":[{{"type":"toolResult","toolCallId":"{call_id}","toolName":"{tool}","text":"{text}"}}]}}}}"#
    )
}

#[test]
fn long_session_units_stay_within_the_token_budget() {
    let units = ingest_pi_session(&multi_cycle_session(), "seg-e2e").unwrap();
    assert!(!units.is_empty(), "one episode for the single user turn");
    for unit in &units {
        assert_eq!(unit.kind, UnitKind::Episode);
        assert_eq!(unit.metadata["episode"], 1);
        assert!(
            unit.token_count <= MAX_TOKENS,
            "oversized turn piece exceeds budget: {}",
            unit.token_count
        );
    }
    assert!(units[0].metadata["pieces"].as_u64().unwrap() > 1);
    assert_eq!(units[0].metadata["session"], "seg-e2e");
    assert!(units[0].routing_text.contains("session: seg-e2e"));
}
#[test]
fn every_piece_carries_the_session_and_touched_file_anchors() {
    let units = ingest_pi_session(&multi_cycle_session(), "seg-e2e").unwrap();
    for unit in &units {
        assert!(unit
            .anchors
            .iter()
            .any(|anchor| anchor.kind == snoop::core::AnchorKind::Session
                && anchor.value == "seg-e2e"));
        assert!(unit
            .anchors
            .iter()
            .any(|anchor| anchor.kind == snoop::core::AnchorKind::File
                && anchor.value == "src/auth.rs"));
    }
}

#[test]
fn bash_outcomes_survive_the_policy_change() {
    let units = ingest_pi_session(&multi_cycle_session(), "seg-e2e").unwrap();
    let outcomes = units[0].metadata["outcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0]["outcome"], "failed");
    assert_eq!(outcomes[1]["outcome"], "passed");
    let evidence: String = units.iter().map(|u| u.evidence_text.as_str()).collect();
    assert!(evidence.contains("Command: cargo test"));
    assert!(evidence.contains("Outcome: failed"));
    assert!(evidence.contains("Outcome: passed"));
    assert!(!evidence.contains("three matching call sites"));
    assert!(!evidence.contains("edit applied"));
}
