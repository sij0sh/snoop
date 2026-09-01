use std::collections::HashSet;

use snoop::store::Store;

pub(crate) fn print_command_error(message: String, hint: Option<&str>) -> ! {
    let mut payload = serde_json::json!({
        "status": "error",
        "error": message,
    });
    if let Some(hint) = hint {
        payload["hint"] = serde_json::json!(hint);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&payload)
            .unwrap_or_else(|_| "{\"status\": \"error\"}".to_string())
    );
    std::process::exit(1)
}

pub(crate) fn print_ensure_error(message: String) -> ! {
    print_command_error(message, None)
}

pub(crate) fn excluded_session_units(
    store: &Store,
    sessions: &[String],
) -> Result<HashSet<i64>, Box<dyn std::error::Error + Send + Sync>> {
    let mut excluded = HashSet::new();
    for session in sessions {
        excluded.extend(store.unit_ids_for_anchor("session", session)?);
    }
    Ok(excluded)
}
