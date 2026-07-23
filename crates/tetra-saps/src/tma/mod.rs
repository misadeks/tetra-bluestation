use tetra_core::{BitBuffer, EndpointId, LinkId, TetraAddress, Todo, TxReporter};

use crate::lcmc::fields::chan_alloc_req::CmceChanAllocReq;

/// Clause 20.4.1.1.1
/// TMA-CANCEL request: this primitive shall be used to cancel a TMA-UNITDATA
/// request primitive that was submitted by the LLC.
#[derive(Debug, Clone)]
pub struct TmaCancelReq {
    pub req_handle: Todo,
}

/// Clause 20.4.1.1.2
/// TMA-RELEASE indication: this primitive may be used when the MAC leaves a
/// channel in order to indicate that the connection on that channel is lost
/// (e.g. to indicate local disconnection of any advanced links on that channel).
#[derive(Debug, Clone)]
pub struct TmaReleaseInd {
    pub endpoint_id: EndpointId,
}

/// Clause 22.3.3.1.1 gives some hints on reports in the MS context
#[derive(Debug, Clone)]
pub enum TmaReport {
    /// Confirm handle to the request
    ConfirmHandle,
    /// MS only. Successful complete transmission by random access
    SuccessRandomAccess,
    /// MS only. Complete transmission by reserved access or stealing
    SuccessReservedOrStealing,

    FailedTransfer,
    FragmentationFailure,
    /// MS only
    RandomAccessFailure,
}

/// Clause 20.4.1.1.3
/// TMA-REPORT indication: this primitive shall be used by the MAC to report
/// on the progress or failure of a request procedure. The result of the
/// transfer shall be passed as a report parameter.
#[derive(Debug, Clone)]
pub struct TmaReportInd {
    pub req_handle: Todo,
    pub report: TmaReport,
}

/// Clause 20.4.1.1.4
/// TMA-UNITDATA request: this primitive shall be used to request the MAC to
/// transmit a TM-SDU.
#[derive(Debug, Clone)]
pub struct TmaUnitdataReq {
    pub req_handle: Todo,
    pub pdu: BitBuffer,
    pub main_address: TetraAddress,
    /// Link ID (0 when no LLC link context exists)
    pub link_id: LinkId,
    // pub scrambling_code: u32, // TODO FIXME : according to the spec, should be there, but why do we need to provide this?
    pub endpoint_id: EndpointId,
    /// L3-assigned PDU priority (ETSI TS 100 392-2 cl. 20.2.4.52). `None` means
    /// no explicit L3 priority was supplied, in which case the MS-MAC uses the
    /// access code's minimum permitted priority so the random-access priority
    /// gate (cl. 23.5.1.4.4) always passes (the pre-plumbing behaviour, used for
    /// LLC-internal acks). `Some(p)` carries the priority (0..=7) the originating
    /// L3 entity assigned to the PDU (e.g. MM registration = 6, cl. 16.4.3).
    pub pdu_priority: Option<u8>,
    /// Emergency indication (cl. 14.5.6 / 23.5.1.4.4). When set, the MS-MAC may
    /// use access code A even below its minimum priority and doubles the maximum
    /// number of random-access transmissions (cl. 23.5.1.4.9). Only set by
    /// emergency CMCE traffic; MM signalling never sets it.
    pub is_emergency: bool,
    pub stealing_permission: bool,
    pub subscriber_class: Todo,
    pub air_interface_encryption: Option<Todo>,
    pub stealing_repeats_flag: Option<bool>,
    pub data_category: Option<Todo>,

    // Custom fields for BS stack:
    /// Optional Channel Allocation Request that may be included by CMCE
    pub chan_alloc: Option<CmceChanAllocReq>,
    pub tx_reporter: Option<TxReporter>,
}

/// Clause 20.4.1.1.4
/// TMA-UNITDATA indication: this primitive shall be used by the MAC to deliver
/// a received TM-SDU. This primitive may also be used with no TM-SDU if the
/// MAC needs to inform the higher layers of a channel allocation received
/// without an associated TM-SDU.
#[derive(Debug, Clone)]
pub struct TmaUnitdataInd {
    pub pdu: Option<BitBuffer>,
    pub main_address: TetraAddress,
    pub scrambling_code: u32,
    pub endpoint_id: EndpointId,
    pub new_endpoint_id: Option<EndpointId>,
    pub css_endpoint_id: Option<EndpointId>,
    pub air_interface_encryption: Todo,
    pub chan_change_response_req: bool,
    pub chan_change_handle: Option<Todo>,
    pub chan_info: Option<Todo>,
}
