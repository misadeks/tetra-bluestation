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
    /// AACH downlink usage marker (ETSI TS 100 392-2 cl. 21.4.7.2) of the slot
    /// this TCH/S block was received on — the traffic-channel identity the
    /// serving cell assigned to the call (cl. 23.5.5). `None` when unknown: the
    /// LMAC/BS producer leaves it `None`; the MS UMAC fills it from the slot's
    /// ACCESS-ASSIGN before relaying the frame up to CMCE. Lets CC-MS
    /// demultiplex concurrent calls (different usage markers on different slots)
    /// to the correct call rather than guessing.
    pub usage_marker: Option<u8>,
    /// SSI (group or individual) the serving cell bound `usage_marker` to for
    /// this MS, resolved by the MS UMAC from the MAC-RESOURCE that carried the
    /// usage-marker assignment (cl. 21.4.3.1 SSI + usage-marker addressing).
    /// `None` on the producer side and on the BS. CC-MS matches it against the
    /// destination address of one of its active calls to attribute the frame.
    pub owner_ssi: Option<u32>,
}
