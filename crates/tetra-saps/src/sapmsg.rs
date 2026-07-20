use core::fmt::Display;

use tetra_core::Sap;
use tetra_core::tetra_entities::TetraEntity;

use crate::control::brew::MmSubscriberUpdate;
use crate::control::call_control::CallControl;
use crate::control::sds::CmceSdsData;
use crate::tmd::TmdCircuitDataInd;
use crate::tmd::TmdCircuitDataReq;
use crate::tnmm::TnmmTestDemand;
use crate::tnmm::TnmmTestResponse;

use super::lcmc::*;
use super::lmm::*;
use super::ltpd::*;
use super::tla::*;
use super::tlmb::*;
use super::tlmc::*;
use super::tma::*;
use super::tmv::*;
use super::tp::*;

/// Exhaustive list of SapMsgType structs for use in the SapMsg struct
/// See Clause 19.2.1 for an overview of all lower-layer SAPs
#[derive(Debug, Clone)]
pub enum SapMsgInner {
    // TODO FIXME and all that stuff
    // PhyControlUpdateNetinfo(PhyControlUpdateNetinfo),

    // LmacControlUpdateNetinfo(LmacControlUpdateNetinfo),
    /// TP-SAP (Contents not defined in standard)
    TpUnitdataInd(TpUnitdataInd),
    TpUnitdataReq(TpUnitdataReqSlot),

    // TMV-SAP
    TmvUnitdataReq(TmvUnitdataReqSlot),
    TmvUnitdataInd(TmvUnitdataInd),
    TmvConfigureReq(TmvConfigureReq),
    TmvConfigureConf(TmvConfigureConf),

    // TMA-SAP
    TmaUnitdataInd(TmaUnitdataInd),
    TmaUnitdataReq(TmaUnitdataReq),
    TmaReportInd(TmaReportInd),

    // TMB-SAP / TLB-SAP (merged to TLMB-SAP)
    TlmbSyncInd(TlmbSyncInd),
    TlmbSysinfoInd(TlmbSysinfoInd),
    /// MS only — internal PHY -> MLE serving-cell downlink monitoring
    /// indication (radio link failure / recovery, cl. 18.3.4.5.3 / 18.3.4.7).
    /// Not an air-interface PDU.
    TlmbMonitorInd(TlmbMonitorInd),

    // TMC-SAP
    TlmcConfigureReq(TlmcConfigureReq),

    // TMD-SAP (Uplane traffic and signalling)
    TmdCircuitDataReq(TmdCircuitDataReq),
    TmdCircuitDataInd(TmdCircuitDataInd),

    // TLB-SAP
    // TlmbSyncInd(TlmbSyncInd),
    // TlmbSysinfoInd(TlmbSysinfoInd),

    // TLA-SAP
    TlaTlDataIndBl(TlaTlDataIndBl),
    TlaTlDataReqBl(TlaTlDataReqBl),
    TlaTlReportInd(TlaTlReportInd),
    TlaTlUnitdataIndBl(TlaTlUnitdataIndBl),
    TlaTlUnitdataReqBl(TlaTlUnitdataReqBl),

    // LMM-SAP (MLE-MM)
    LmmMleUnitdataInd(LmmMleUnitdataInd),
    LmmMleUnitdataReq(LmmMleUnitdataReq),
    /// MLE -> MM confirmation that a cell has been selected with the required
    /// characteristics (ETSI TS 100 392-2 cl. 17.3.2). Carries whether the
    /// serving cell requires registration (from D-MLE-SYSINFO, cl. 18.4.2.2).
    LmmMleActivateConf(LmmMleActivateConf),
    /// MM -> MLE identities request (cl. 17.3.2): the set of identities by which
    /// the MS is currently known (own ISSI + attached GSSIs). The MLE configures
    /// the MAC downlink address filter (cl. 23.4.1.2.1) from it.
    LmmMleIdentitiesReq(LmmMleIdentitiesReq),

    // LCMC-SAP (MLE-CMCE)
    LcmcMleUnitdataInd(LcmcMleUnitdataInd),
    LcmcMleUnitdataReq(LcmcMleUnitdataReq),
    /// MLE -> CMCE break indication (cl. 17.3.3): communication resources are
    /// temporarily unavailable (serving-cell radio link failure).
    LcmcMleBreakInd(LcmcMleBreakInd),
    /// MLE -> CMCE reopen indication (cl. 17.3.3): communication resources are
    /// available again (serving-cell downlink recovered).
    LcmcMleReopenInd(LcmcMleReopenInd),

    // CMCE -> UMAC control
    CmceCallControl(CallControl),

    // MM -> Brew/CMCE subscriber update
    MmSubscriberUpdate(MmSubscriberUpdate),

    // CMCE SDS <-> Brew SDS routing
    CmceSdsData(CmceSdsData),

    // LTPD-SAP (MLE-LTPD)
    LtpdMleUnitdataInd(LtpdMleUnitdataInd),

    // TNMM-SAP (MM-User)
    TnmmTestDemand(TnmmTestDemand),
    TnmmTestResponse(TnmmTestResponse),
}

impl Display for SapMsgInner {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            // TP-SAP
            // SapMsgInner::TpUnitdataInd(_) => write!(f, "TpUnitdataInd"),

            // TMV-SAP
            SapMsgInner::TmvUnitdataReq(_) => write!(f, "TmvUnitdataReq"),
            SapMsgInner::TmvUnitdataInd(_) => write!(f, "TmvUnitdataInd"),
            SapMsgInner::TmvConfigureReq(_) => write!(f, "TmvConfigureReq"),
            SapMsgInner::TmvConfigureConf(_) => write!(f, "TmvConfigureConf"),

            // TMA-SAP
            SapMsgInner::TmaUnitdataInd(_) => write!(f, "TmaUnitdataInd"),
            SapMsgInner::TmaUnitdataReq(_) => write!(f, "TmaUnitdataReq"),

            // TMB-SAP
            SapMsgInner::TlmbSyncInd(_) => write!(f, "TmbSyncInd"),
            SapMsgInner::TlmbSysinfoInd(_) => write!(f, "TmbSysinfoInd"),
            SapMsgInner::TlmbMonitorInd(_) => write!(f, "TlmbMonitorInd"),

            // LCMC-SAP
            SapMsgInner::LcmcMleBreakInd(_) => write!(f, "LcmcMleBreakInd"),
            SapMsgInner::LcmcMleReopenInd(_) => write!(f, "LcmcMleReopenInd"),

            // LMM-SAP
            SapMsgInner::LmmMleIdentitiesReq(_) => write!(f, "LmmMleIdentitiesReq"),

            // Control/Brew
            SapMsgInner::MmSubscriberUpdate(_) => write!(f, "MmSubscriberUpdate"),

            // TLB-SAP
            // SapMsgInner::TlbTlSyncInd(_) => write!(f, "TlbTlSyncInd"),
            // SapMsgInner::TlbTlSysinfoInd(_) => write!(f, "TlbTlSysinfoInd"),
            _ => panic!("Unknown SapMsgInner type"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SapMsg {
    pub sap: Sap,
    pub src: TetraEntity,
    pub dest: TetraEntity,
    pub msg: SapMsgInner,
}

impl SapMsg {
    pub fn new(sap: Sap, src: TetraEntity, dest: TetraEntity, msg: SapMsgInner) -> Self {
        Self { sap, src, dest, msg }
    }

    pub fn get_source(&self) -> &TetraEntity {
        &self.src
    }
    pub fn get_dest(&self) -> &TetraEntity {
        &self.dest
    }
    pub fn get_sap(&self) -> &Sap {
        &self.sap
    }
    // pub fn get_prim(&self) -> &SapPrim {
    //     &self.prim
    // }
    // pub fn get_subprim(&self) -> &SapSubPrim {
    //     &self.subprim
    // }
}
