use std::any::Any;
use core::fmt::Display;

use crate::common::tetra_common::Sap;
use crate::common::tetra_entities::TetraEntity;
use crate::common::tdma_time::TdmaTime;

use super::lcmc::*;
use super::lmm::*;
use super::ltpd::*;
use super::tla::*;
use super::tlmb::*;
use super::tlmc::*;
use super::tma::*;
use super::tmv::*;
use super::tp::*;
use super::tnsds::*;
use super::dgna::*;


pub trait SapMsgT: Any + Send {
    fn as_any(&self) -> &dyn Any;

    fn get_sap(&self) -> &Sap;
    // fn get_prim(&self) -> &SapPrim;
    // fn get_subprim(&self) -> &SapSubPrim;

    fn get_source(&self) -> &TetraEntity;
    fn get_dest(&self) -> &TetraEntity;

    fn serialize(&self);
    fn deserialize(&self);
}

/// Exhaustive list of SapMsgType structs for use in the SapMsg struct
/// See Clause 19.2.1 for an overview of all lower-layer SAPs
/// 
#[derive(Debug)]
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

    // TMB-SAP / TLB-SAP (merged to TLMB-SAP)
    TlmbSyncInd(TlmbSyncInd),
    TlmbSysinfoInd(TlmbSysinfoInd),

    // TMC-SAP
    TlmcConfigureReq(TlmcConfigureReq),
    // TMD-SAP (Uplane traffic and signalling)



    
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

    // LCMC-SAP (MLE-CMCE)
    LcmcMleUnitdataInd(LcmcMleUnitdataInd),
    LcmcMleUnitdataReq(LcmcMleUnitdataReq),

    // TN-SDS-SAP (User/CMCE)
    TnsdsUnitdataReq(TnsdsUnitdataReq),
    TnsdsUnitdataInd(TnsdsUnitdataInd),

    // Internal UI -> MM request (DGNA)
    DgnaSetReq(DgnaSetReq),

    // LTPD-SAP (MLE-LTPD)
    LtpdMleUnitdataInd(LtpdMleUnitdataInd),
    LtpdMleUnitdataReq(LtpdMleUnitdataReq)
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

            SapMsgInner::DgnaSetReq(_) => write!(f, "DgnaSetReq"),
            SapMsgInner::LtpdMleUnitdataInd(_) => write!(f, "LtpdMleUnitdataInd"),
            SapMsgInner::LtpdMleUnitdataReq(_) => write!(f, "LtpdMleUnitdataReq"),

            // TLB-SAP
            // SapMsgInner::TlbTlSyncInd(_) => write!(f, "TlbTlSyncInd"),
            // SapMsgInner::TlbTlSysinfoInd(_) => write!(f, "TlbTlSysinfoInd"),
            _ => write!(f, "SapMsgInner"),
        }
    }
}

#[derive(Debug)]
pub struct SapMsg {
    pub sap: Sap,
    // pub prim: SapPrim,
    // pub subprim: SapSubPrim,
    pub src: TetraEntity,
    pub dest: TetraEntity,
    /// Downlink time at the time the message was created
    pub dltime: TdmaTime,
    // pub t_action: TdmaTime,

    pub msg: SapMsgInner
}

impl SapMsg {
    pub fn new(
        sap: Sap,
        // prim: SapPrim,
        // subprim: SapSubPrim,
        src: TetraEntity,
        dest: TetraEntity,
        t_submit: TdmaTime,
        // t_action: TdmaTime,
        msg: SapMsgInner
    ) -> Self {
        Self {
            sap,
            // prim,
            // subprim,
            src,
            dest,
            dltime: t_submit,
            // t_action,
            msg
        }
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
