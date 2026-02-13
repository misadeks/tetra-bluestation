pub mod sapmsg;

pub mod tp;
pub mod tpc;

pub mod tmv;

pub mod tma;
/// TMB-SAP and TLB-SAP are effectively the same. 
/// As such, they are merged here
pub mod tlmb;

pub mod tla;
// pub mod tlb; implemented in tmbtlb
pub mod tlmc;
pub mod tle;

pub mod lmm;
pub mod lcmc;
pub mod ltpd; 

pub mod tnsds;

// Internal UI -> MM message for DGNA control
pub mod dgna;
pub mod sn;
