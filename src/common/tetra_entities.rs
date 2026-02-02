// Entities as used in the standard
#[derive(PartialEq, Eq, Hash, Clone, Debug, Copy)]
pub enum TetraEntity {
    /// Physical layer
    Phy, 
    /// Lower MAC layer
    Lmac, 
    /// Upper MAC layer
    Umac, 
    /// Logical link control
    Llc,  
    /// Mobile Link Entity
    Mle,
    /// Mobility Management
    Mm,
    /// Circuit Mode Control Entity 
    Cmce,
    /// SubNetwork Dependent Convergence Protocol
    Sndcp,
    /// PEI/AT serial interface (non-ETSI entity)
    PeiAt,
    /// External command server (non-ETSI entity)
    CmdServer,
    /// HTTP UI bridge (non-ETSI entity)
    HttpUi,
}
