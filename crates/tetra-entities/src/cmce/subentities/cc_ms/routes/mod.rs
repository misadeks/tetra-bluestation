use super::*;

// CC-MS ingress routes, grouped by the SAP the messages arrive on
// (mirrors the cc_bs `routes/` layout):
//   * `rd`       — LCMC-SAP lower boundary: air-interface downlink CMCE PDUs
//                  delivered up from MLE (LcmcMleUnitdataInd), cl. 14.
//   * `tncc_sap` — TNCC-SAP upper boundary: user-application call-control
//                  primitives (TNCC-SETUP/-TX/-RELEASE/answer), cl. 11.3.3.
mod rd;
mod tncc_sap;
