use snoop::core::UnitKind;
use snoop::ingest::harness::{
    ingest_pi_session, MAX_EPISODES_PER_SESSION, SEGMENTATION_POLICY_VERSION,
};
use snoop::ingest::units::MAX_TOKENS;

const LONG_SENTENCE: &str = "Walk the auth module top down, quote every relevant span, and explain the reasoning behind each step in enough detail for a later review. ";

fn long_text() -> String {
    LONG_SENTENCE.repeat(4)
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
        r#"{{"type":"message","id":"{id}","message":{{"role":"toolResult","toolCallId":"{call_id}","toolName":"{tool}","content":[{{"type":"toolResult","toolCallId":"{call_id}","toolName":"{tool}","text":"{text}"}}]}}}}"#
    )
}
#[test]
fn multi_cycle_session_segments_into_work_cycles() {
    let (_, units) = ingest_pi_session(&multi_cycle_session(), "seg-e2e").unwrap();
    assert_eq!(
        units.len(),
        3,
        "boundary after investigation and after the failed validation"
    );
    let reasons: Vec<&str> = units
        .iter()
        .map(|unit| unit.metadata["boundary_reason"].as_str().unwrap())
        .collect();
    assert_eq!(
        reasons,
        [
            "investigate_to_modify",
            "failed_validation_to_edit",
            "episode_end"
        ]
    );
    let phases: Vec<&str> = units
        .iter()
        .map(|unit| unit.metadata["phase"].as_str().unwrap())
        .collect();
    assert_eq!(phases, ["investigate", "validate", "resolve"]);

    let failed = &units[1];
    assert!(
        failed.evidence_text.contains("Outcome: failed"),
        "the modification and its failed validation must be co-located"
    );
    assert!(failed.evidence_text.contains("edit src/auth.rs"));
    assert!(units[2].evidence_text.contains("Outcome: passed"));

    for unit in &units {
        assert_eq!(unit.kind, UnitKind::EpisodeSegment);
        assert!(unit.token_count <= MAX_TOKENS);
        assert_eq!(unit.metadata["policy_version"], SEGMENTATION_POLICY_VERSION);
    }
}

#[test]
fn candidate_boundaries_are_recorded_for_every_boundary() {
    let (atoms, _) = ingest_pi_session(&multi_cycle_session(), "seg-e2e").unwrap();
    let candidates = atoms[0].metadata["candidate_boundaries"]
        .as_array()
        .unwrap();
    assert_eq!(candidates.len(), 6, "one record per legal boundary");
    let selected: Vec<&str> = candidates
        .iter()
        .filter(|candidate| candidate["selected"] == true)
        .map(|candidate| candidate["boundary_id"].as_str().unwrap())
        .collect();
    assert_eq!(selected, ["boundary_2", "boundary_4"]);
    let failed_flags: Vec<bool> = candidates
        .iter()
        .map(|candidate| {
            candidate["features"]["failed_validation"]
                .as_bool()
                .unwrap()
        })
        .collect();
    assert_eq!(
        failed_flags,
        [false, false, false, true, false, false],
        "only the boundary after the failed validation carries the flag"
    );
    for candidate in candidates {
        assert!(candidate["features"]["phase_transition"]
            .as_str()
            .unwrap()
            .contains("->"));
        assert!(candidate["features"]["left_tokens"].is_u64());
    }
}

#[test]
fn segment_ranges_partition_the_episode() {
    let (atoms, _) = ingest_pi_session(&multi_cycle_session(), "seg-e2e").unwrap();
    let segments = atoms[0].metadata["segments"].as_array().unwrap();
    assert_eq!(segments.len(), 3);
    for pair in segments.windows(2) {
        assert_eq!(
            pair[0]["end_byte"].as_u64().unwrap(),
            pair[1]["start_byte"].as_u64().unwrap(),
            "child ranges must be contiguous inside one episode"
        );
    }
    assert!(
        segments[0]["start_byte"].as_u64().unwrap() < segments[2]["end_byte"].as_u64().unwrap()
    );
}

#[test]
fn appending_events_leaves_sealed_segments_unchanged() {
    let head = format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}",
        r#"{"type":"session","version":3,"id":"seg-append","cwd":"/tmp/repo"}"#,
        line_user("u1", "Investigate the refresh loop"),
        line_call("a1", "c1", "edit", r#""path":"src/auth.rs""#),
        line_result("r1", "c1", "edit", "ok"),
        line_call("a2", "c2", "bash", r#""command":"cargo test""#),
        line_result("r2", "c2", "bash", r#"{\"exitCode\":1}"#),
        line_text("a3", "The first attempt failed and needs a different fix."),
    );
    let tail = format!(
        "\n{}\n{}",
        line_user("u2", "Try the rotation approach instead"),
        line_text("a4", "Rotation fixed the loop and the tests pass now."),
    );
    let before = ingest_pi_session(&head, "seg-append").unwrap();
    let after = ingest_pi_session(&format!("{head}{tail}"), "seg-append").unwrap();
    assert_eq!(before.1.len(), 1, "one open segment before the append");
    assert_eq!(after.1.len(), 2, "the append starts a new episode");

    let sealed: std::collections::HashSet<&str> = before
        .1
        .iter()
        .map(|unit| unit.metadata["segment_id"].as_str().unwrap())
        .collect();
    let resealed: std::collections::HashSet<&str> = after
        .1
        .iter()
        .map(|unit| unit.metadata["segment_id"].as_str().unwrap())
        .collect();
    assert!(
        sealed.is_subset(&resealed),
        "sealed segment ids must survive the append"
    );
    let before_hashes: std::collections::HashSet<&str> = before
        .1
        .iter()
        .map(|unit| unit.content_hash.as_str())
        .collect();
    let after_hashes: std::collections::HashSet<&str> = after
        .1
        .iter()
        .map(|unit| unit.content_hash.as_str())
        .collect();
    assert!(before_hashes.is_subset(&after_hashes));
}

#[test]
fn episode_cap_retains_the_newest_episodes() {
    let mut lines =
        vec![r#"{"type":"session","version":3,"id":"seg-cap","cwd":"/tmp/repo"}"#.to_string()];
    for index in 0..210 {
        lines.push(line_user(
            &format!("u{index}"),
            &format!("User turn number {index} with enough detail to index properly."),
        ));
    }
    let (atoms, units) = ingest_pi_session(&lines.join("\n"), "seg-cap").unwrap();
    assert_eq!(atoms.len(), MAX_EPISODES_PER_SESSION);
    assert_eq!(units.len(), MAX_EPISODES_PER_SESSION);
    let first_episode = atoms[0].metadata["episode"].as_u64().unwrap();
    assert_eq!(first_episode, 11, "the oldest ten episodes are dropped");
    assert_eq!(
        atoms[0].breadcrumb, "pi-session:seg-cap > episode 11",
        "retained episodes keep their absolute numbering"
    );
    let last_episode = atoms.last().unwrap().metadata["episode"].as_u64().unwrap();
    assert_eq!(last_episode, 210);
}

#[test]
fn oversized_single_message_splits_into_bounded_pieces() {
    let huge = "x".repeat(5_000);
    let session = format!(
        "{}\n{}",
        r#"{"type":"session","version":3,"id":"seg-huge","cwd":"/tmp/repo"}"#,
        line_text("a1", &huge),
    );
    let (_, units) = ingest_pi_session(&session, "seg-huge").unwrap();
    assert!(units.len() > 1, "an oversized single message must split");
    assert!(
        units.iter().all(|unit| unit.token_count <= MAX_TOKENS),
        "every piece stays inside the hard maximum"
    );
    assert!(units
        .iter()
        .all(|unit| { unit.metadata["boundary_reason"] == "message_split_last_resort" }));
}

#[test]
fn segmentation_is_deterministic_across_runs() {
    let first = ingest_pi_session(&multi_cycle_session(), "seg-e2e").unwrap();
    let second = ingest_pi_session(&multi_cycle_session(), "seg-e2e").unwrap();
    assert_eq!(first.0, second.0, "atoms must be identical across runs");
    assert_eq!(first.1, second.1, "units must be identical across runs");
}
