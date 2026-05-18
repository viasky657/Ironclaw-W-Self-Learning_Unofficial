use crate::types::{AgentInfo, Message};

/// Build a minimal conversation snapshot from the agent state.
///
/// `AgentInfo` is a plain Rust struct — no arbitrary dict access.
/// Only the fields we actually need are included.
pub fn build_minimal_snapshot(agent_info: &AgentInfo, messages: &[Message]) -> serde_json::Value {
    let recent: Vec<serde_json::Value> = messages
        .iter()
        .rev()
        .take(10)
        .rev()
        .map(|m| {
            serde_json::json!({
                "role": m.role,
                // Truncate to 2048 chars to keep snapshot size bounded.
                "content": truncate_str(&m.content, 2048),
            })
        })
        .collect();

    serde_json::json!({
        "session_id": agent_info.session_id,
        "model": agent_info.model,
        "provider": agent_info.provider,
        "timestamp": chrono_utc_now(),
        "recent_messages": recent,
    })
}

fn truncate_str(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        s
    } else {
        // Truncate at a char boundary.
        let mut idx = max_chars;
        while !s.is_char_boundary(idx) {
            idx -= 1;
        }
        &s[..idx]
    }
}

fn chrono_utc_now() -> String {
    // Use std::time for a simple ISO-8601 timestamp without pulling in chrono.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Format as RFC 3339 / ISO 8601 UTC.
    let (y, mo, d, h, mi, s) = epoch_to_ymd_hms(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, s)
}

/// Minimal epoch → (year, month, day, hour, min, sec) conversion.
fn epoch_to_ymd_hms(epoch: u64) -> (u64, u64, u64, u64, u64, u64) {
    let s = epoch % 60;
    let m = (epoch / 60) % 60;
    let h = (epoch / 3600) % 24;
    let days = epoch / 86400;

    // Gregorian calendar calculation.
    let mut year = 1970u64;
    let mut remaining = days;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        year += 1;
    }
    let months = [31u64, if is_leap(year) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 1u64;
    for &days_in_month in &months {
        if remaining < days_in_month {
            break;
        }
        remaining -= days_in_month;
        month += 1;
    }
    (year, month, remaining + 1, h, m, s)
}

fn is_leap(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AgentInfo;

    fn make_agent() -> AgentInfo {
        AgentInfo {
            session_id: "test-session".to_string(),
            provider: "anthropic".to_string(),
            model: "claude-3-5-sonnet".to_string(),
            base_url: None,
            recent_messages: vec![],
        }
    }

    #[test]
    fn snapshot_contains_required_fields() {
        let agent = make_agent();
        let msgs = vec![
            Message { role: "user".to_string(), content: "hello".to_string() },
            Message { role: "assistant".to_string(), content: "hi".to_string() },
        ];
        let snap = build_minimal_snapshot(&agent, &msgs);
        assert_eq!(snap["session_id"], "test-session");
        assert_eq!(snap["provider"], "anthropic");
        assert_eq!(snap["model"], "claude-3-5-sonnet");
        assert!(snap["timestamp"].as_str().unwrap().ends_with('Z'));
        assert_eq!(snap["recent_messages"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn snapshot_truncates_long_content() {
        let agent = make_agent();
        let long_content = "x".repeat(4096);
        let msgs = vec![Message { role: "user".to_string(), content: long_content }];
        let snap = build_minimal_snapshot(&agent, &msgs);
        let content = snap["recent_messages"][0]["content"].as_str().unwrap();
        assert!(content.len() <= 2048);
    }

    #[test]
    fn snapshot_limits_to_10_messages() {
        let agent = make_agent();
        let msgs: Vec<Message> = (0..20)
            .map(|i| Message { role: "user".to_string(), content: format!("msg {}", i) })
            .collect();
        let snap = build_minimal_snapshot(&agent, &msgs);
        assert_eq!(snap["recent_messages"].as_array().unwrap().len(), 10);
    }
}
