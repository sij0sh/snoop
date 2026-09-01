#!/usr/bin/env python3
"""Scan pi session files for get_repo_context calls and analyze result quality."""
import json, re, sys, time
from collections import Counter
from pathlib import Path

SESSIONS = Path.home() / ".pi" / "agent" / "sessions"
NOW = time.time()

def parse_packet(text):
    if not text:
        return None
    text = text.strip()
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        pass
    start = text.find("{")
    end = text.rfind("}")
    while start >= 0 and end > start:
        try:
            return json.loads(text[start:end+1])
        except json.JSONDecodeError:
            end = text.rfind("}", start, end)
    return None

def norm(s):
    return re.sub(r"\s+", " ", s or "").strip().lower()

def calls_in_file(path):
    """Two passes: collect toolResults, then pair with assistant toolCalls."""
    entries = []
    with open(path, encoding="utf-8", errors="replace") as fh:
        for line in fh:
            try:
                entries.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    sid, cwd = None, None
    for e in entries:
        if e.get("type") == "session":
            sid, cwd = e.get("id"), e.get("cwd")
            break
    results = {}
    for e in entries:  # pass 1: collect tool results
        if e.get("type") != "message":
            continue
        msg = e.get("message") or {}
        if msg.get("role") == "toolResult" and msg.get("toolName") == "get_repo_context":
            texts = [b.get("text", "") for b in msg.get("content", []) if b.get("type") == "text"]
            results[msg.get("toolCallId")] = "\n".join(texts)
    last_user = None
    calls = []
    for e in entries:  # pass 2: chronological calls with prior-user tracking
        if e.get("type") != "message":
            continue
        msg = e.get("message") or {}
        role = msg.get("role")
        if role == "user":
            texts = [b.get("text", "") for b in msg.get("content", []) if b.get("type") == "text"]
            t = "\n".join(x for x in texts if x).strip()
            if t and not t.startswith("<"):
                last_user = t
        elif role == "assistant":
            for b in msg.get("content", []):
                if b.get("type") == "toolCall" and b.get("name") == "get_repo_context":
                    calls.append({
                        "session_id": sid, "cwd": cwd,
                        "call_ts": e.get("timestamp"),
                        "query": (b.get("arguments") or {}).get("query"),
                        "max_tokens": (b.get("arguments") or {}).get("max_tokens"),
                        "result_text": results.get(b.get("id")),
                        "prev_user_text": last_user,
                        "file": str(path),
                    })
    return calls

def analyze(calls, days):
    stats = Counter()
    details = []
    for s in calls:
        stats["calls_total"] += 1
        pkt = parse_packet(s["result_text"])
        if pkt is None or "items" not in pkt:
            stats["unparseable_results"] += 1
            continue
        items = pkt["items"]
        stats["packets_parsed"] += 1
        stats["items_total"] += len(items)
        for i in items:
            stats[f"items_kind_{i.get('source_kind')}"] += 1

        self_hits = [i for i in items
                     if i.get("source_kind") == "AgentSession" and s["session_id"]
                     and (i.get("source_locator") or "").startswith(f"pi-session:{s['session_id']}")]
        if self_hits:
            stats["calls_with_self_session_hits"] += 1
            stats["self_hit_items"] += len(self_hits)
        # rank of first self hit (1-based)
        if self_hits:
            first_self = min(items.index(h) for h in self_hits) + 1
            stats["self_hit_best_rank_sum"] += first_self
            stats["self_hit_best_rank_n"] += 1
            if first_self <= 3:
                stats["self_hit_in_top3"] += 1

        top = items[0] if items else None
        if top and top.get("source_kind") == "AgentSession":
            stats["top_item_is_agent_session"] += 1
            if top in self_hits:
                stats["top_item_is_self_session"] += 1
            if s["prev_user_text"]:
                pu = norm(s["prev_user_text"])[:300]
                ev = norm(top.get("evidence_text"))[:800]
                if pu and pu[:150] in ev:
                    stats["top_item_echoes_a_user_prompt"] += 1

        # commit-hunk flooding: items sharing one git locator
        loc_counts = Counter(i.get("source_locator") for i in items if i.get("source_kind") == "GitCommit")
        if loc_counts:
            dom_loc, dom_n = loc_counts.most_common(1)[0]
            if dom_n >= 3:
                stats["packets_with_commit_flooding(>=3 same-commit items)"] += 1
            stats["max_items_single_commit_sum"] += dom_n
        # repeated identical evidence texts (exact dupes that survived dedup via different hashes? or same hash admitted?)
        ev_counts = Counter(norm(i.get("evidence_text"))[:200] for i in items)
        reps = sum(n - 1 for n in ev_counts.values() if n > 1)
        if reps:
            stats["packets_with_repeated_evidence_texts"] += 1
            stats["repeated_evidence_item_count"] += reps
        # epoch timestamps in last position of each item
        ts_present = sum(1 for i in items if i.get("timestamp") is not None)
        stats["items_with_timestamp"] += ts_present
        details.append((s, pkt, self_hits, loc_counts))
    return stats, details

def main():
    days = float(sys.argv[1]) if len(sys.argv) > 1 else 30.0
    cutoff = NOW - days * 86400
    files = []
    for d in SESSIONS.iterdir():
        if d.is_dir():
            for f in d.glob("*.jsonl"):
                try:
                    if f.stat().st_mtime >= cutoff:
                        files.append(f)
                except OSError:
                    pass
    files.sort(key=lambda f: f.stat().st_mtime, reverse=True)
    calls = []
    nfiles = 0
    for f in files:
        got = calls_in_file(f)
        if got:
            nfiles += 1
        calls.extend(got)

    print(f"=== SCAN: {nfiles} session files w/ calls (mtime within {days:.0f}d), "
          f"{len(calls)} get_repo_context calls ===\n")
    stats, details = analyze(calls, days)
    for k in ["calls_total", "packets_parsed", "unparseable_results", "items_total",
              "items_with_timestamp", "calls_with_self_session_hits", "self_hit_items",
              "self_hit_in_top3", "top_item_is_agent_session", "top_item_is_self_session",
              "top_item_echoes_a_user_prompt",
              "packets_with_commit_flooding(>=3 same-commit items)",
              "max_items_single_commit_sum", "packets_with_repeated_evidence_texts",
              "repeated_evidence_item_count"]:
        if stats.get(k):
            print(f"{k}: {stats[k]}")
    if stats.get("self_hit_best_rank_n"):
        print(f"avg best self-hit rank: {stats['self_hit_best_rank_sum']/stats['self_hit_best_rank_n']:.1f}")
    print()
    for k in sorted(stats):
        if k.startswith("items_kind_"):
            print(f"{k}: {stats[k]}")

    print("\n=== PER-CALL DETAIL (all calls) ===")
    for s, pkt, self_hits, loc_counts in details:
        print("-" * 90)
        q = (s["query"] or "")[:100]
        print(f"ts={s['call_ts']} tokens={pkt.get('token_count')}/{pkt.get('budget')} items={len(pkt['items'])} query={q!r}")
        for rank, i in enumerate(pkt["items"][:8], 1):
            loc = i.get("source_locator", "")
            short = loc.replace("pi-session:", "ps:").replace("git:", "git:")
            mark = " <<< SELF" if i in self_hits else ""
            print(f"  #{rank:2d} {i.get('source_kind'):12s} ts={i.get('timestamp')} {short[:58]}{mark}")
        flood = [f"{loc[:20]}x{n}" for loc, n in loc_counts.most_common(3) if n >= 3]
        if flood:
            print(f"  commit flooding: {', '.join(flood)}")

if __name__ == "__main__":
    main()
