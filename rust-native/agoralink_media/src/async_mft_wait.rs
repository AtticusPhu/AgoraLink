use std::fmt;
use std::time::Duration;

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AsyncMftWaitKind {
    NeedInput,
    HaveOutput,
    Drain,
}

impl AsyncMftWaitKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::NeedInput => "need-input",
            Self::HaveOutput => "have-output",
            Self::Drain => "drain",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AsyncMftWaitFailure {
    Timeout {
        kind: AsyncMftWaitKind,
        timeout: Duration,
    },
    Cancelled {
        kind: AsyncMftWaitKind,
    },
}

impl fmt::Display for AsyncMftWaitFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout { kind, timeout } => write!(
                formatter,
                "async MFT {} wait timed out after {} ms",
                kind.name(),
                timeout.as_millis()
            ),
            Self::Cancelled { kind } => {
                write!(formatter, "async MFT {} wait was cancelled", kind.name())
            }
        }
    }
}

/// Recognizes only messages emitted by the typed MFT cancellation variant.
/// This deliberately does not treat arbitrary strings containing "cancelled"
/// as a normal shutdown.
pub fn is_typed_cancellation_message(message: &str) -> bool {
    [
        AsyncMftWaitKind::NeedInput,
        AsyncMftWaitKind::HaveOutput,
        AsyncMftWaitKind::Drain,
    ]
    .into_iter()
    .any(|kind| AsyncMftWaitFailure::Cancelled { kind }.to_string() == message)
}

#[derive(Debug)]
pub enum AsyncMftPollError<E> {
    Wait(AsyncMftWaitFailure),
    Source(E),
}

/// Polls an asynchronous MFT without ever entering a blocking GetEvent call.
/// The clock and backoff are injected so timeout/cancellation tests need no sleeps.
pub fn poll_until<T, E, Now, Cancel, Poll, Backoff>(
    kind: AsyncMftWaitKind,
    timeout: Duration,
    mut elapsed: Now,
    mut cancelled: Cancel,
    mut poll: Poll,
    mut backoff: Backoff,
) -> Result<T, AsyncMftPollError<E>>
where
    Now: FnMut() -> Duration,
    Cancel: FnMut() -> bool,
    Poll: FnMut() -> Result<Option<T>, E>,
    Backoff: FnMut(),
{
    loop {
        if cancelled() {
            return Err(AsyncMftPollError::Wait(AsyncMftWaitFailure::Cancelled {
                kind,
            }));
        }
        if let Some(value) = poll().map_err(AsyncMftPollError::Source)? {
            return Ok(value);
        }
        if elapsed() >= timeout {
            return Err(AsyncMftPollError::Wait(AsyncMftWaitFailure::Timeout {
                kind,
                timeout,
            }));
        }
        backoff();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn missing_event_returns_typed_timeout_without_sleeping() {
        for kind in [
            AsyncMftWaitKind::NeedInput,
            AsyncMftWaitKind::HaveOutput,
            AsyncMftWaitKind::Drain,
        ] {
            let elapsed_ms = Cell::new(0u64);
            let result = poll_until::<(), (), _, _, _, _>(
                kind,
                Duration::from_millis(10),
                || Duration::from_millis(elapsed_ms.get()),
                || false,
                || Ok(None),
                || elapsed_ms.set(elapsed_ms.get() + 2),
            );
            assert!(matches!(
                result,
                Err(AsyncMftPollError::Wait(AsyncMftWaitFailure::Timeout {
                    kind: observed,
                    ..
                })) if observed == kind
            ));
        }
    }

    #[test]
    fn event_arriving_at_deadline_is_accepted() {
        let elapsed_ms = Cell::new(0u64);
        let polls = Cell::new(0u32);
        let result = poll_until(
            AsyncMftWaitKind::HaveOutput,
            Duration::from_millis(10),
            || Duration::from_millis(elapsed_ms.get()),
            || false,
            || {
                polls.set(polls.get() + 1);
                Ok::<_, ()>((polls.get() == 6).then_some(42u32))
            },
            || elapsed_ms.set(elapsed_ms.get() + 2),
        );
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn cancellation_wins_without_polling_source() {
        for kind in [
            AsyncMftWaitKind::NeedInput,
            AsyncMftWaitKind::HaveOutput,
            AsyncMftWaitKind::Drain,
        ] {
            let polls = Cell::new(0u32);
            let result = poll_until::<(), (), _, _, _, _>(
                kind,
                Duration::from_secs(3),
                Duration::default,
                || true,
                || {
                    polls.set(polls.get() + 1);
                    Ok(None)
                },
                || {},
            );
            assert!(matches!(
                result,
                Err(AsyncMftPollError::Wait(AsyncMftWaitFailure::Cancelled {
                    kind: observed
                })) if observed == kind
            ));
            assert_eq!(polls.get(), 0);
        }
    }

    #[test]
    fn cancellation_message_classifier_is_exact() {
        assert!(is_typed_cancellation_message(
            "async MFT need-input wait was cancelled"
        ));
        assert!(is_typed_cancellation_message(
            "async MFT drain wait was cancelled"
        ));
        assert!(!is_typed_cancellation_message("operation cancelled"));
        assert!(!is_typed_cancellation_message(
            "async MFT need-input wait timed out after 100 ms"
        ));
    }

    #[test]
    fn ctrl_c_during_need_input_wait_returns_cancelled() {
        let result = poll_until::<(), (), _, _, _, _>(
            AsyncMftWaitKind::NeedInput,
            Duration::from_secs(3),
            Duration::default,
            || true,
            || Ok(None),
            || {},
        );
        assert!(matches!(
            result,
            Err(AsyncMftPollError::Wait(AsyncMftWaitFailure::Cancelled {
                kind: AsyncMftWaitKind::NeedInput
            }))
        ));
    }

    #[test]
    fn ctrl_c_during_drain_wait_returns_cancelled() {
        let result = poll_until::<(), (), _, _, _, _>(
            AsyncMftWaitKind::Drain,
            Duration::from_secs(3),
            Duration::default,
            || true,
            || Ok(None),
            || {},
        );
        assert!(matches!(
            result,
            Err(AsyncMftPollError::Wait(AsyncMftWaitFailure::Cancelled {
                kind: AsyncMftWaitKind::Drain
            }))
        ));
    }

    #[test]
    fn session_peer_timeout_cancels_need_input_without_waiting_for_deadline() {
        let token = crate::shutdown::CancellationToken::new();
        let poll_count = Cell::new(0u32);
        let result = poll_until::<(), (), _, _, _, _>(
            AsyncMftWaitKind::NeedInput,
            Duration::from_secs(3),
            Duration::default,
            || token.is_cancelled(),
            || {
                poll_count.set(poll_count.get() + 1);
                Ok(None)
            },
            || {
                token.cancel(crate::shutdown::StopReason::PeerTimeout);
            },
        );
        assert!(matches!(
            result,
            Err(AsyncMftPollError::Wait(AsyncMftWaitFailure::Cancelled {
                kind: AsyncMftWaitKind::NeedInput
            }))
        ));
        assert_eq!(
            token.reason(),
            Some(crate::shutdown::StopReason::PeerTimeout)
        );
        assert_eq!(poll_count.get(), 1);
    }
}
