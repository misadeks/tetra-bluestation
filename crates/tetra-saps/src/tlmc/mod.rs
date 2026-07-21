use tetra_core::{EndpointId, Todo};

#[derive(Debug, Clone)]
pub struct TlmcAssessmentInd;

#[derive(Debug, Clone)]
pub struct TlmcAssessmentListReq;

/// MS only — runtime downlink retune request (**[impl policy]**).
///
/// MLE -> upper-MAC hop of the MLE-owned cell-selection / scan retune path
/// (MLE -> UMAC (TLMC) -> LMAC (TMV) -> PHY (TPC)). UMAC forwards it down to
/// LMAC as a `TmvTuneReq`. The standard scanning/selection vehicle is the
/// TMC-SAP scan/select service (cl. 20.4.3); this direct tune models the
/// physical retune those procedures require. Not an over-the-air primitive.
#[derive(Debug, Clone)]
pub struct TlmcTuneReq {
    /// Absolute downlink centre frequency to tune to, in Hz.
    pub carrier_hz: u32,
}

#[derive(Debug, Clone)]
pub struct TlmcCellReadInd;
#[derive(Debug, Clone)]
pub struct TlmcCellReadConf;

/// Clause 20.4.3 and 20.3.5.4.1c
/// TMC-CONFIGURE indication: MAY BE USED BY LLC. this primitive shall be used to indicate loss of lower layer
/// resources. It may be used to indicate regain of lower layer resources.
#[derive(Debug, Clone)]
pub struct TlmcConfigureInd {
    pub endpoint_id: EndpointId,
    pub lower_layer_resource_availability: Todo,
}

#[derive(Debug, Clone)]
/// Poorly documented, but used in TL-CONFIGURE. Signals which addresses are valid,
/// including full MCC/MNC.
///
/// Extended (MS mode) to also carry the layer-2 downlink address-filter set the
/// MLE derives from the MS's identities (own ISSI + attached GSSIs, cl. 17.3.2 /
/// 23.4.1.2.1). The MAC accepts downlink traffic addressed to any of these. The
/// SSI members are `None` for a scrambling-only configure (e.g. at cell
/// selection, when only MCC/MNC are being set) so an existing filter is left
/// unchanged.
pub struct TlmcValidAddress {
    pub mcc: u16,
    pub mnc: u16,
    /// The MS's own individual identity (ISSI). `None` leaves the MAC's current
    /// individual identity unchanged.
    pub individual_ssi: Option<u32>,
    /// The full set of group identities (GSSIs) currently attached. `Some`
    /// replaces the MAC's accepted-group set; `None` leaves it unchanged.
    pub group_ssis: Option<Vec<u32>>,
}
/// Clause 20.4.3 and 20.3.5.4.1c
/// TMC-CONFIGURE request, see below. When used by MLE:
/// TL-CONFIGURE request, confirm: this primitive shall be used to set up and configure the layer 2 according to the
/// chosen cell parameters and the current state of the MS. It may also be used to provide the LLC and MAC with
/// retransmission strategy in graceful service degradation mode. The parameters shall be as defined in table 20.36.
#[derive(Debug, Clone, Default)]
pub struct TlmcConfigureReq {
    pub threshold_values: Option<Todo>,
    pub distribution_on_18th_frame: Option<Todo>,
    pub scch_information: Option<Todo>,
    pub energy_economy_group: Option<Todo>,
    pub energy_economy_startpoint: Option<Todo>,
    pub dual_watch_energy_economy_group: Option<Todo>,
    pub dual_watch_startpoint: Option<Todo>,
    pub mle_activity_indicator: Option<Todo>,
    pub channel_change_accepted: Option<Todo>,
    pub channel_change_handle: Option<Todo>,
    pub operating_mode: Option<Todo>,
    pub call_release: Option<Todo>,
    pub valid_addresses: Option<TlmcValidAddress>,
    pub ms_default_data_priority: Option<Todo>,
    pub layer_2_data_priority_lifetime: Option<Todo>,
    pub layer_2_data_priority_signalling_delay: Option<Todo>,
    pub data_priority_random_access_delay_factor: Option<Todo>,
    pub schedule_repetition_information: Option<Todo>,
    pub data_class_activity_information: Option<Todo>,
    pub endpoint_id: Option<Todo>,
    pub periodic_reporting_timer: Option<Todo>,
    pub graceful_service_degradation_mode_control: Option<Todo>,
}

/// Clause 20.4.3
/// TMC-CONFIGURE request: this primitive shall be used to accept or reject a channel change. It is also used for the
/// LLC to provide the MAC with information about activity. It is used for the LLC to provide the MAC with timer
/// information that may be needed in the napping procedure. It may also be used for the LLC to provide the MAC with
/// information that the MAC may use to make choices about link adaptation. It may also be used to provide the MAC with
/// retransmission strategy in graceful service degradation mode. The parameters shall be as defined in table 20.57.
// #[derive(Debug, Clone)]
// pub struct TmcTlConfigureReq {
//     pub channel_change_handle: Option<Todo>,
//     pub channel_change_accepted: Option<bool>,
//     pub mle_activity_indicator: Option<Todo>,
//     pub llc_timer_status: Option<Todo>,
//     pub link_performance_info: Option<Todo>,
//     pub endpoint_identifier: Option<EndpointId>,
//     pub graceful_service_degradation_mode_control: Option<Todo>,
// }

/// 20.3.5.4.1c TL-CONFIGURE primitive
/// TL-CONFIGURE request, confirm: this primitive shall be used to set up and configure the layer 2 according to the
/// chosen cell parameters and the current state of the MS. It may also be used to provide the LLC and MAC with
/// retransmission strategy in graceful service degradation mode. The parameters shall be as defined in table 20.36.
#[derive(Debug, Clone)]
pub struct TlmcConfigureConf {
    pub threshold_values: Option<Todo>,
    pub distribution_on_18th_frame: Option<Todo>,
    pub scch_information: Option<Todo>,
    pub energy_economy_group: Option<Todo>,
    pub energy_economy_startpoint: Option<Todo>,
    pub dual_watch_energy_economy_group: Option<Todo>,
    pub dual_watch_startpoint: Option<Todo>,
    pub operating_mode: Option<Todo>,
    pub call_release: Option<Todo>,
    pub valid_addresses: Option<Todo>,
    pub ms_default_data_priority: Option<Todo>,
    pub layer_2_data_priority_lifetime: Option<Todo>,
    pub layer_2_data_priority_signalling_delay: Option<Todo>,
    pub data_priority_random_access_delay_factor: Option<Todo>,
    pub schedule_repetition_information: Option<Todo>,
    pub data_class_activity_information: Option<Todo>,
    pub endpoint_id: Option<Todo>,
}

#[derive(Debug, Clone)]
pub struct TlmcMeasurementInd;

#[derive(Debug, Clone)]
pub struct TlmcMonitorInd;

#[derive(Debug, Clone)]
pub struct TlmcMonitorListReq;

#[derive(Debug, Clone)]
pub struct TlmcReportInd;

#[derive(Debug, Clone)]
pub struct TlmcScanReq;
#[derive(Debug, Clone)]
pub struct TlmcScanConf;

#[derive(Debug, Clone)]
pub struct TlmcScanReportInd;

#[derive(Debug, Clone)]
pub struct TlmcSelectReq;
#[derive(Debug, Clone)]
pub struct TlmcSelectInd;
#[derive(Debug, Clone)]
pub struct TlmcSelectResp;
#[derive(Debug, Clone)]
pub struct TlmcSelectConf;

// Clause 20.4.3

// The TMC-SAP shall be used for the transfer of local layer management information. It does not provide data transfer
// services over the air interface. The request and response primitives at the TLC-SAP shall be directly mapped as request
// and response primitives at the TMC-SAP, and the indication and confirm primitives at the TMC-SAP shall be directly
// transported to the TLC-SAP as indication and confirm primitives. The service descriptions for the TLC-SAP are
// therefore valid for the TMC-SAP and are not repeated. The LLC also may use the TMC-CONFIGURE request
// primitive.
