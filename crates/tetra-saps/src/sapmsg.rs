use core::fmt::Display;

use tetra_core::Sap;
use tetra_core::tetra_entities::TetraEntity;

use crate::control::brew::MmSubscriberUpdate;
use crate::control::call_control::CallControl;
use crate::control::sds::CmceSdsData;
use crate::tmd::TmdCircuitDataInd;
use crate::tmd::TmdCircuitDataReq;
use crate::tncc::*;
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
use super::tpc::*;

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

    /// TPC-SAP (PHY <-> LMAC management) — MS runtime downlink retune.
    TpcTuneReq(TpcTuneReq),
    /// TPC-SAP (PHY <-> LMAC management) — MS runtime uplink (TX) retune.
    TpcTxTuneReq(TpcTxTuneReq),

    // TMV-SAP
    TmvUnitdataReq(TmvUnitdataReqSlot),
    TmvUnitdataInd(TmvUnitdataInd),
    TmvConfigureReq(TmvConfigureReq),
    TmvConfigureConf(TmvConfigureConf),
    /// TMV-SAP — MS runtime downlink retune (UMAC -> LMAC).
    TmvTuneReq(TmvTuneReq),
    /// TMV-SAP — MS runtime uplink (TX) retune (UMAC -> LMAC).
    TmvTxTuneReq(TmvTxTuneReq),

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
    /// MS only — internal PHY -> MLE scan-dwell-elapsed indication used by the
    /// scanning cell-selection engine during acquisition (cl. 18.3.4). Not an
    /// air-interface PDU.
    TlmbScanDwellInd(TlmbScanDwellInd),

    // TMC-SAP
    TlmcConfigureReq(TlmcConfigureReq),
    /// TMC-SAP — MS runtime downlink retune (MLE -> UMAC).
    TlmcTuneReq(TlmcTuneReq),
    /// TMC-SAP — MS U-plane transmit configuration (MLE -> UMAC): completes the
    /// MLE-CONFIGURE (cl. 17.3.3) hop for U-plane switching / Tx-grant so the
    /// upper MAC knows whether this MS may emit uplink TCH/S traffic (cl.
    /// 14.5.1.4 / cl. 23).
    TlmcUPlaneConfigureReq(TlmcUPlaneConfigureReq),

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
    /// MLE -> MM serving-cell receive-level indication (implementation-defined,
    /// Plane B; NOT an ETSI LMM primitive). Carries the current serving-cell
    /// downlink RSSI so MM can surface it in the management runtime state.
    LmmMleRssiInd(LmmMleRssiInd),
    /// MLE -> MM break indication (cl. 18.3.3 / 18.3.4.5.3): communication
    /// resources are temporarily unavailable (serving-cell radio link failure).
    /// MM goes out of service and emits TNMM-SERVICE "out of service".
    LmmMleBreakInd(LmmMleBreakInd),
    /// MLE -> MM reopen indication (cl. 18.3.4.7): communication resources are
    /// available again (serving-cell downlink recovered).
    LmmMleReopenInd(LmmMleReopenInd),

    // Operator-driven cell survey / selection (implementation-defined, Plane B;
    // NOT ETSI LMM primitives). See `crate::lmm`.
    /// MM -> MLE: switch between automatic and manual cell selection.
    LmmMleSelectionModeReq(LmmMleSelectionModeReq),
    /// MM -> MLE: start/stop a receive-only survey of candidate carriers.
    LmmMleScanReq(LmmMleScanReq),
    /// MM -> MLE: camp (and optionally register) on a chosen candidate carrier.
    LmmMleCampReq(LmmMleCampReq),
    /// MLE -> MM: one cell found during a survey.
    LmmMleScanResultInd(LmmMleScanResultInd),
    /// MLE -> MM: the survey pass finished.
    LmmMleScanCompleteInd(LmmMleScanCompleteInd),

    // LCMC-SAP (MLE-CMCE)
    LcmcMleUnitdataInd(LcmcMleUnitdataInd),
    LcmcMleUnitdataReq(LcmcMleUnitdataReq),
    /// CMCE -> MLE lower-layer circuit-mode configuration (cl. 17.3.3);
    /// CC-MS uses this for U-plane switching per cl. 14.5.1.4.
    LcmcMleConfigureReq(LcmcMleConfigureReq),
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

    // TNCC-SAP (CMCE/CC-User), ETSI TS 100 392-2 v3.10.1 cl. 11.3.3.
    TnccAlertIndication(TnccAlertIndication),
    TnccCompleteRequest(TnccCompleteRequest),
    TnccCompleteIndication(TnccCompleteIndication),
    TnccCompleteConfirm(TnccCompleteConfirm),
    TnccDtmfRequest(TnccDtmfRequest),
    TnccDtmfIndication(TnccDtmfIndication),
    TnccModifyRequest(TnccModifyRequest),
    TnccModifyIndication(TnccModifyIndication),
    TnccNotifyIndication(TnccNotifyIndication),
    TnccProceedIndication(TnccProceedIndication),
    TnccReleaseRequest(TnccReleaseRequest),
    TnccReleaseIndication(TnccReleaseIndication),
    TnccReleaseConfirm(TnccReleaseConfirm),
    TnccSetupRequest(TnccSetupRequest),
    TnccSetupIndication(TnccSetupIndication),
    TnccSetupResponse(TnccSetupResponse),
    TnccSetupConfirm(TnccSetupConfirm),
    TnccTxRequest(TnccTxRequest),
    TnccTxIndication(TnccTxIndication),
    TnccTxConfirm(TnccTxConfirm),
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
            SapMsgInner::TmvTuneReq(_) => write!(f, "TmvTuneReq"),
            SapMsgInner::TmvTxTuneReq(_) => write!(f, "TmvTxTuneReq"),
            SapMsgInner::TpcTuneReq(_) => write!(f, "TpcTuneReq"),
            SapMsgInner::TpcTxTuneReq(_) => write!(f, "TpcTxTuneReq"),

            // TMA-SAP
            SapMsgInner::TmaUnitdataInd(_) => write!(f, "TmaUnitdataInd"),
            SapMsgInner::TmaUnitdataReq(_) => write!(f, "TmaUnitdataReq"),

            // TMB-SAP
            SapMsgInner::TlmbSyncInd(_) => write!(f, "TmbSyncInd"),
            SapMsgInner::TlmbSysinfoInd(_) => write!(f, "TmbSysinfoInd"),
            SapMsgInner::TlmbMonitorInd(_) => write!(f, "TlmbMonitorInd"),
            SapMsgInner::TlmbScanDwellInd(_) => write!(f, "TlmbScanDwellInd"),

            // TMC-SAP
            SapMsgInner::TlmcTuneReq(_) => write!(f, "TlmcTuneReq"),
            SapMsgInner::TlmcUPlaneConfigureReq(_) => write!(f, "TlmcUPlaneConfigureReq"),

            // LCMC-SAP
            SapMsgInner::LcmcMleBreakInd(_) => write!(f, "LcmcMleBreakInd"),
            SapMsgInner::LcmcMleReopenInd(_) => write!(f, "LcmcMleReopenInd"),
            SapMsgInner::LcmcMleConfigureReq(_) => write!(f, "LcmcMleConfigureReq"),

            // LMM-SAP
            SapMsgInner::LmmMleIdentitiesReq(_) => write!(f, "LmmMleIdentitiesReq"),
            SapMsgInner::LmmMleRssiInd(_) => write!(f, "LmmMleRssiInd"),
            SapMsgInner::LmmMleBreakInd(_) => write!(f, "LmmMleBreakInd"),
            SapMsgInner::LmmMleReopenInd(_) => write!(f, "LmmMleReopenInd"),
            SapMsgInner::LmmMleSelectionModeReq(_) => write!(f, "LmmMleSelectionModeReq"),
            SapMsgInner::LmmMleScanReq(_) => write!(f, "LmmMleScanReq"),
            SapMsgInner::LmmMleCampReq(_) => write!(f, "LmmMleCampReq"),
            SapMsgInner::LmmMleScanResultInd(_) => write!(f, "LmmMleScanResultInd"),
            SapMsgInner::LmmMleScanCompleteInd(_) => write!(f, "LmmMleScanCompleteInd"),

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
