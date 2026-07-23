use std::io::{self, BufRead};
use std::thread::{self, JoinHandle};

use crate::shutdown::{CancellationToken, StopReason};

const LOCAL_STOP_TYPE: &str = "LOCAL_STOP";
const LOCAL_STOP_VERSION: u64 = 1;

pub(crate) fn spawn_stdin_listener(
    cancellation: CancellationToken,
) -> Result<JoinHandle<()>, String> {
    thread::Builder::new()
        .name("agoralink-local-control".to_string())
        .spawn(move || {
            let stdin = io::stdin();
            let _ = consume_local_control(stdin.lock(), &cancellation);
        })
        .map_err(|error| format!("spawn local control listener failed: {error}"))
}

fn consume_local_control<R: BufRead>(
    reader: R,
    cancellation: &CancellationToken,
) -> Result<bool, String> {
    for line in reader.lines() {
        let line = line.map_err(|error| format!("read local control failed: {error}"))?;
        match parse_local_stop(&line)? {
            LocalControlAction::Ignore => continue,
            LocalControlAction::Stop => {
                cancellation.cancel(StopReason::LocalStop);
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LocalControlAction {
    Ignore,
    Stop,
}

fn parse_local_stop(text: &str) -> Result<LocalControlAction, String> {
    let command_type = json_string_field(text, "type");
    if command_type.as_deref() != Some(LOCAL_STOP_TYPE) {
        return Ok(LocalControlAction::Ignore);
    }
    if json_u64_field(text, "version") != Some(LOCAL_STOP_VERSION) {
        return Err("LOCAL_STOP version must be 1".to_string());
    }
    let reason = json_string_field(text, "reason").unwrap_or_default();
    if !matches!(reason.as_str(), "gui_stop" | "app_close" | "local_stop") {
        return Err("LOCAL_STOP reason is invalid".to_string());
    }
    Ok(LocalControlAction::Stop)
}

fn field_value_start<'a>(text: &'a str, field: &str) -> Option<&'a str> {
    let needle = format!(r#""{field}""#);
    let (_, tail) = text.split_once(&needle)?;
    let tail = tail.trim_start();
    let tail = tail.strip_prefix(':')?;
    Some(tail.trim_start())
}

fn json_string_field(text: &str, field: &str) -> Option<String> {
    let value = field_value_start(text, field)?.strip_prefix('"')?;
    let end = value.find('"')?;
    Some(value[..end].to_string())
}

fn json_u64_field(text: &str, field: &str) -> Option<u64> {
    let value = field_value_start(text, field)?;
    let digits = value
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn valid_gui_stop_cancels_shared_token() {
        let token = CancellationToken::new();
        let input =
            Cursor::new(b"{\"type\":\"LOCAL_STOP\",\"reason\":\"gui_stop\",\"version\":1}\n");
        assert_eq!(consume_local_control(input, &token), Ok(true));
        assert_eq!(token.reason(), Some(StopReason::LocalStop));
    }

    #[test]
    fn unrelated_commands_are_ignored() {
        let token = CancellationToken::new();
        let input = Cursor::new(b"{\"type\":\"PING\",\"version\":1}\n");
        assert_eq!(consume_local_control(input, &token), Ok(false));
        assert!(!token.is_cancelled());
    }

    #[test]
    fn malformed_local_stop_is_rejected_without_cancellation() {
        let token = CancellationToken::new();
        let input =
            Cursor::new(b"{\"type\":\"LOCAL_STOP\",\"reason\":\"gui_stop\",\"version\":2}\n");
        assert!(consume_local_control(input, &token).is_err());
        assert!(!token.is_cancelled());
    }
}
