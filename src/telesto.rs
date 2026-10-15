//! The Telesto operation runner: one op in flight at a time over a ctrl + data channel pair.
//! Async I/O and timeouts are new; the completion rules are the ported
//! [`crate::haversine::controller`].
//!
//! Generic over [`Link`] so the logic is testable without a radio; [`crate::client`]
//! provides the BLE implementation.

use std::future::Future;
use std::time::Duration;

use tokio::time::{timeout_at, Instant};

use crate::haversine::controller::{ControllerError, Transfer};
use crate::protocol::{
    decode_ctrl_response, CtrlResponse, ParseError, TelestoError, TelestoOp, TelestoRequest,
};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// One notification from the ring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notification {
    Ctrl(Vec<u8>),
    Data(Vec<u8>),
}

/// The transport under the runner: write to the two characteristics, receive notifications.
pub trait Link {
    type Error: std::error::Error + Send + Sync + 'static;

    fn write_ctrl(&mut self, bytes: &[u8]) -> impl Future<Output = Result<(), Self::Error>> + Send;
    fn write_data(&mut self, bytes: &[u8]) -> impl Future<Output = Result<(), Self::Error>> + Send;
    /// Next notification; `None` when the link has closed.
    fn next_notification(&mut self) -> impl Future<Output = Option<Notification>> + Send;
    /// Drop any notifications already queued (stale data from an earlier op).
    fn drain(&mut self);
    /// Maximum bytes per data-channel write.
    fn chunk_size(&self) -> usize;
}

#[derive(Debug, thiserror::Error)]
pub enum OpError<E: std::error::Error + 'static> {
    #[error(transparent)]
    Link(E),
    #[error(transparent)]
    Telesto(#[from] TelestoError),
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error("telesto controller error {0:?}")]
    Controller(ControllerError),
    #[error("telesto op {0:?} timed out after {1:?}")]
    Timeout(TelestoOp, Duration),
    #[error("link closed during telesto op")]
    Closed,
    #[error("payload is only valid for PROGRAM_MEMORY")]
    PayloadNotAllowed,
    #[error("payload is {0} bytes but the request length is {1}")]
    PayloadLength(usize, u32),
}

/// Run one Telesto operation and return the ctrl response and the data received.
///
/// The request header goes to ctrl, then a PROGRAM payload (exactly `request.length` bytes)
/// goes to data in `chunk_size` writes. Notifications are fed to the ported native state
/// machine ([`Transfer`]): ctrl fragments accumulate to 12 bytes, and a READ completes once
/// the response has arrived and `data_length` (response bytes 8..12) data bytes are in;
/// excess data is dropped.
pub async fn run_operation<L: Link>(
    link: &mut L,
    request: TelestoRequest,
    payload: Option<&[u8]>,
    timeout: Duration,
) -> Result<(CtrlResponse, Vec<u8>), OpError<L::Error>> {
    match payload {
        Some(_) if request.op != TelestoOp::ProgramMemory => {
            return Err(OpError::PayloadNotAllowed)
        }
        Some(p) if p.len() != request.length as usize => {
            return Err(OpError::PayloadLength(p.len(), request.length))
        }
        _ => {}
    }

    link.drain();
    link.write_ctrl(&request.encode())
        .await
        .map_err(OpError::Link)?;
    if let Some(p) = payload {
        for chunk in p.chunks(link.chunk_size().max(1)) {
            link.write_data(chunk).await.map_err(OpError::Link)?;
        }
    }

    let deadline = Instant::now() + timeout;
    let mut transfer = Transfer::new(&request);
    let mut buf = Vec::new();
    let status = loop {
        let n = timeout_at(deadline, link.next_notification())
            .await
            .map_err(|_| OpError::Timeout(request.op, timeout))?
            .ok_or(OpError::Closed)?;
        let done = match n {
            Notification::Ctrl(c) => transfer.on_ctrl(&c),
            Notification::Data(d) => transfer.on_data(&d).map(|(kept, done)| {
                buf.extend_from_slice(kept);
                done
            }),
        }
        .map_err(OpError::Controller)?;
        if let Some(s) = done {
            break s;
        }
    };
    let resp = decode_ctrl_response(transfer.ctrl_response().expect("complete implies response"))?;
    debug_assert_eq!(resp.status, status);
    resp.check()?;
    Ok((resp, buf))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::convert::Infallible;

    use super::*;
    use crate::protocol::{TelestoError, TelestoOp};

    /// Scripted ring: each ctrl write queues the next scripted notification list.
    struct FakeRing {
        script: VecDeque<Vec<Notification>>,
        queue: VecDeque<Notification>,
        ctrl_writes: Vec<Vec<u8>>,
        data_writes: Vec<Vec<u8>>,
    }

    impl FakeRing {
        fn new(script: Vec<Vec<Notification>>) -> Self {
            Self {
                script: script.into(),
                queue: VecDeque::new(),
                ctrl_writes: vec![],
                data_writes: vec![],
            }
        }
    }

    impl Link for FakeRing {
        type Error = Infallible;

        async fn write_ctrl(&mut self, bytes: &[u8]) -> Result<(), Infallible> {
            assert_eq!(bytes.len(), 13);
            self.ctrl_writes.push(bytes.to_vec());
            self.queue
                .extend(self.script.pop_front().expect("unscripted op"));
            Ok(())
        }

        async fn write_data(&mut self, bytes: &[u8]) -> Result<(), Infallible> {
            self.data_writes.push(bytes.to_vec());
            Ok(())
        }

        async fn next_notification(&mut self) -> Option<Notification> {
            match self.queue.pop_front() {
                Some(n) => Some(n),
                None => std::future::pending().await,
            }
        }

        fn drain(&mut self) {
            self.queue.clear();
        }

        fn chunk_size(&self) -> usize {
            20
        }
    }

    fn ctrl(status: u32, data_length: u32) -> Notification {
        let mut v = status.to_le_bytes().to_vec();
        v.extend([0; 4]);
        v.extend(data_length.to_le_bytes());
        Notification::Ctrl(v)
    }

    fn data(d: &[u8]) -> Vec<Notification> {
        d.chunks(17)
            .map(|c| Notification::Data(c.to_vec()))
            .collect()
    }

    async fn run(
        ring: &mut FakeRing,
        req: TelestoRequest,
    ) -> Result<(CtrlResponse, Vec<u8>), OpError<Infallible>> {
        run_operation(ring, req, None, DEFAULT_TIMEOUT).await
    }

    #[tokio::test]
    async fn read_response_then_chunked_data() {
        let payload: Vec<u8> = (0..=255).collect();
        let mut script = vec![ctrl(0, 256)];
        script.extend(data(&payload));
        let mut ring = FakeRing::new(vec![script]);
        let req = TelestoRequest::read(0x4003_0006, 256);
        let (resp, got) = run(&mut ring, req).await.unwrap();
        assert_eq!((resp.status, resp.data_length), (0, 256));
        assert_eq!(got, payload);
        assert_eq!(ring.ctrl_writes[0], req.encode().to_vec());
    }

    #[tokio::test]
    async fn data_before_response_then_response() {
        let mut script = data(&[1; 100]);
        script.push(ctrl(0, 100));
        let mut ring = FakeRing::new(vec![script]);
        let (_, got) = run(&mut ring, TelestoRequest::read(0x4002_0000, 0))
            .await
            .unwrap();
        assert_eq!(got.len(), 100);
    }

    #[tokio::test]
    async fn excess_after_response_dropped() {
        let mut script = vec![ctrl(0, 4)];
        script.extend(data(&[7; 40]));
        let mut ring = FakeRing::new(vec![script]);
        let (_, got) = run(&mut ring, TelestoRequest::read(0x4003_0005, 4))
            .await
            .unwrap();
        assert_eq!(got, vec![7; 4]);
    }

    #[tokio::test]
    async fn fragmented_ctrl_response() {
        let Notification::Ctrl(c) = ctrl(0, 0) else {
            unreachable!()
        };
        let script = vec![
            Notification::Ctrl(c[..5].to_vec()),
            Notification::Ctrl(c[5..].to_vec()),
        ];
        let mut ring = FakeRing::new(vec![script]);
        run(&mut ring, TelestoRequest::read(0x4003_0005, 0))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn error_status_is_returned() {
        let mut ring = FakeRing::new(vec![vec![ctrl(0x43, 0)]]);
        let err = run(&mut ring, TelestoRequest::read(0x4002_0000, 0))
            .await
            .unwrap_err();
        assert!(matches!(err, OpError::Telesto(TelestoError(0x43))));
    }

    #[tokio::test]
    async fn program_payload_is_chunked_to_data() {
        let mut ring = FakeRing::new(vec![vec![ctrl(0, 0)]]);
        let payload = vec![9u8; 45];
        let req = TelestoRequest::new(TelestoOp::ProgramMemory, 0x4000_0000, 0, 45);
        run_operation(&mut ring, req, Some(&payload), DEFAULT_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(
            ring.data_writes.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![20, 20, 5]
        );
    }

    #[tokio::test]
    async fn payload_rejected_for_reads_and_wrong_length() {
        let mut ring = FakeRing::new(vec![]);
        let err = run_operation(
            &mut ring,
            TelestoRequest::read(0, 4),
            Some(b"x"),
            DEFAULT_TIMEOUT,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, OpError::PayloadNotAllowed));
        let req = TelestoRequest::new(TelestoOp::ProgramMemory, 0x4000_0000, 0, 2);
        let err = run_operation(&mut ring, req, Some(b"x"), DEFAULT_TIMEOUT)
            .await
            .unwrap_err();
        assert!(matches!(err, OpError::PayloadLength(1, 2)));
    }

    #[tokio::test(start_paused = true)]
    async fn silent_ring_times_out() {
        let mut ring = FakeRing::new(vec![vec![]]);
        let err = run_operation(
            &mut ring,
            TelestoRequest::read(0, 4),
            None,
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, OpError::Timeout(TelestoOp::ReadMemory, _)));
    }

    #[tokio::test(start_paused = true)]
    async fn short_read_waits_for_remaining_data() {
        let mut ring = FakeRing::new(vec![vec![ctrl(0, 8), Notification::Data(vec![1; 4])]]);
        let err = run_operation(
            &mut ring,
            TelestoRequest::read(0, 8),
            None,
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, OpError::Timeout(..)));
    }
}
