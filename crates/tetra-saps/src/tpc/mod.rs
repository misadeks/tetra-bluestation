//! TPC-SAP (PHY <-> LMAC management, ETSI TS 100 392-2 cl. 19.2.1): local
//! physical-layer control that does not carry air-interface data.

/// MS only — runtime downlink retune request (**[impl policy]**).
///
/// Commands the physical layer to tune its receiver to a new downlink carrier
/// frequency at runtime. It is the lowest hop of the MLE-owned cell-selection /
/// scan retune path (MLE -> UMAC (TLMC) -> LMAC (TMV) -> PHY (TPC)); the PHY
/// applies it to the SDR via the device retune hook. The standard vehicle for
/// scanning/selection is the TMC-SAP scan/select service (cl. 20.4.3); this
/// direct tune models the physical retune those procedures ultimately require
/// and is not itself an over-the-air primitive.
#[derive(Debug, Clone)]
pub struct TpcTuneReq {
    /// Absolute downlink centre frequency to tune to, in Hz.
    pub carrier_hz: u32,
}
