//! Single-operation state machine of the native `TelestoController`
//! (`TelestoController_receiveCtrlBytes` @0x10cad4, `_receiveDataBytes` @0x10cb74, completion
//! check `FUN_0010c880` @0x10c880). `tests/telesto_vectors.rs` checks it against the real
//! library driven with the same event sequences.
//!
//! Only the receive side is modelled: the request header is [`TelestoRequest::encode`] and a
//! PROGRAM payload is sent whole after it; cancellation and the operation queue are not ported.

use crate::protocol::TelestoRequest;

/// Full ctrl response size: `{u32 status, u32, u32 data_length}` LE.
pub const CTRL_RESPONSE_LEN: usize = 12;

/// Controller error callbacks (TelestoControllerErrorDescriptions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerError {
    /// Code 1: ctrl bytes with no operation, or more than 12 in total.
    UnexpectedCtrl = 1,
    /// Code 2: data bytes while the operation is not a READ.
    UnexpectedData = 2,
}

/// Receive-side state of one Telesto operation.
#[derive(Debug, Clone)]
pub struct Transfer {
    op: u8,
    ctrl: Vec<u8>,
    received: u64,
    status: Option<u32>,
}

impl Transfer {
    pub fn new(request: &TelestoRequest) -> Self {
        Self {
            op: request.op as u8,
            ctrl: Vec::with_capacity(CTRL_RESPONSE_LEN),
            received: 0,
            status: None,
        }
    }

    /// Completion status once finished (0 = success).
    pub fn status(&self) -> Option<u32> {
        self.status
    }

    /// The full 12-byte ctrl response, once received.
    pub fn ctrl_response(&self) -> Option<&[u8]> {
        (self.ctrl.len() == CTRL_RESPONSE_LEN).then_some(&self.ctrl[..])
    }

    fn word(&self, i: usize) -> u32 {
        u32::from_le_bytes(self.ctrl[i * 4..i * 4 + 4].try_into().unwrap())
    }

    /// Ctrl notification. Fragments are accumulated up to 12 bytes.
    pub fn on_ctrl(&mut self, bytes: &[u8]) -> Result<Option<u32>, ControllerError> {
        if self.status.is_some() || bytes.len() > CTRL_RESPONSE_LEN - self.ctrl.len() {
            return Err(ControllerError::UnexpectedCtrl);
        }
        self.ctrl.extend_from_slice(bytes);
        Ok(self.check_complete())
    }

    /// Data notification. Returns the bytes handed to the operation: everything before the
    /// ctrl response arrives, then capped at the response's data length (excess is dropped
    /// with "received too many bytes"). The cap uses the library's unsigned wraparound.
    pub fn on_data<'a>(
        &mut self,
        bytes: &'a [u8],
    ) -> Result<(&'a [u8], Option<u32>), ControllerError> {
        // Library accepts data in state READ (3), or CANCEL (4) after a READ; not modelled: cancel.
        if self.status.is_some() || self.op != 3 {
            return Err(ControllerError::UnexpectedData);
        }
        let room = if self.ctrl.len() == CTRL_RESPONSE_LEN {
            (self.word(2) as u64).wrapping_sub(self.received)
        } else {
            u64::MAX
        };
        let n = (bytes.len() as u64).min(room) as usize;
        self.received += n as u64;
        Ok((&bytes[..n], self.check_complete()))
    }

    fn check_complete(&mut self) -> Option<u32> {
        if self.ctrl.len() != CTRL_RESPONSE_LEN {
            return None;
        }
        if self.op == 3 && self.received < self.word(2) as u64 {
            return None;
        }
        self.status = Some(self.word(0));
        self.status
    }
}
