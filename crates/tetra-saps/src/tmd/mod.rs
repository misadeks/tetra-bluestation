/// Pass TMD circuit data to UMAC for TX scheduling
#[derive(Debug, Clone)]
pub struct TmdCircuitDataReq {
    // call_id: CallId,
    pub ts: u8,
    pub data: Vec<u8>,
}

/// Rx'ed traffic
#[derive(Debug, Clone)]
pub struct TmdCircuitDataInd {
    // call_id: CallId,
    pub ts: u8,
    pub data: Vec<u8>,
    /// Bad-frame indicator: `true` when the channel-decode CRC failed for this
    /// TCH/S block (ETSI TS 100 392-2 cl. 19.4). The frame is still forwarded so
    /// the speech decoder can apply error concealment (EN 300 395-2 substitution
    /// and muting); it must not be treated as valid speech.
    pub bfi: bool,
}
