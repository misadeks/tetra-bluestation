use std::collections::{BTreeSet, VecDeque};
use std::panic;

use tetra_config::bluestation::{SharedConfig, StackMode};
use tetra_core::freqs::FreqInfo;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{
    BitBuffer, PhyBlockNum, PhysicalChannel, Sap, SsiType, TdmaTime, TetraAddress, Todo, TxReporter,
    unimplemented_log,
};
use tetra_saps::tlmb::{TlmbSyncInd, TlmbSysinfoInd};
use tetra_saps::tma::TmaUnitdataInd;
use tetra_saps::tmv::TmvConfigureReq;
use tetra_saps::tmv::TmvTuneReq;
use tetra_saps::tmv::TmvTxTuneReq;
use tetra_saps::tmv::enums::logical_chans::LogicalChannel;
use tetra_saps::tmv::{TmvUnitdataReq, TmvUnitdataReqSlot};
use tetra_saps::{SapMsg, SapMsgInner};

use tetra_pdus::umac::enums::broadcast_type::BroadcastType;
use tetra_pdus::umac::enums::mac_pdu_type::MacPduType;
use tetra_pdus::umac::enums::basic_slotgrant_cap_alloc::BasicSlotgrantCapAlloc;
use tetra_pdus::umac::enums::basic_slotgrant_granting_delay::BasicSlotgrantGrantingDelay;
use tetra_pdus::umac::enums::reservation_requirement::ReservationRequirement;
use tetra_pdus::umac::fields::basic_slotgrant::BasicSlotgrant;
use tetra_pdus::umac::pdus::access_assign::AccessAssign;
use tetra_pdus::umac::pdus::access_assign_fr18::AccessAssignFr18;
use tetra_pdus::umac::pdus::access_define::AccessDefine;
use tetra_pdus::umac::pdus::mac_access::MacAccess;
use tetra_pdus::umac::pdus::mac_data::MacData;
use tetra_pdus::umac::pdus::mac_end_dl::MacEndDl;
use tetra_pdus::umac::pdus::mac_end_hu::MacEndHu;
use tetra_pdus::umac::pdus::mac_end_ul::MacEndUl;
use tetra_pdus::umac::pdus::mac_frag_dl::MacFragDl;
use tetra_pdus::umac::pdus::mac_frag_ul::MacFragUl;
use tetra_pdus::umac::pdus::mac_resource::MacResource;
use tetra_pdus::umac::pdus::mac_sync::MacSync;
use tetra_pdus::umac::pdus::mac_sysinfo::MacSysinfo;
use tetra_pdus::umac::fields::channel_allocation::ChanAllocElement;
use tetra_saps::lcmc::enums::alloc_type::ChanAllocType;
use tetra_saps::lcmc::enums::ul_dl_assignment::UlDlAssignment;

use crate::umac::subcomp::fillbits;
use crate::umac::subcomp::ms_defrag::MsDefrag;
use crate::umac::subcomp::ms_random_access::{
    AccessCode, AccessParamStore, MsRandomAccess, RaAction, Subslot, ThreadRaRng,
    interpret_access_assign,
};
use crate::{MessagePrio, MessageQueue, TetraEntityTrait};

/// SCH/HU (Control Uplink Burst) type-1 MAC block length in bits (ETSI TS 100
/// 392-2, SCH/HU coding parameters).
const SCH_HU_TYPE1_BITS: usize = 92;

/// MAC-END-HU fixed header length in bits (ETSI TS 100 392-2 cl. 21.4.2.2):
/// PDU type (1) + fill-bit indication (1) + length-indication-or-capacity-
/// request flag (1) + length indication (4).
const MAC_END_HU_HEADER_BITS: usize = 7;

/// SCH/F (Normal Uplink Burst) type-1 MAC block length in bits (ETSI TS 100
/// 392-2 SCH/F coding parameters — the same 268-bit block as the downlink
/// SCH/F). Used for a reserved-access MAC-END-UL completing a fragmented
/// uplink transfer whose remainder is too large for a MAC-END-HU subslot.
const SCH_F_TYPE1_BITS: usize = 268;

/// MAC-END-UL fixed header length in bits (ETSI TS 100 392-2 cl. 21.4.2.5):
/// PDU type (2) + PDU subtype (1) + fill-bit indication (1) + length-indication-
/// or-capacity-request (6).
const MAC_END_UL_HEADER_BITS: usize = 10;

/// MAC-FRAG-UL fixed header length in bits (ETSI TS 100 392-2 cl. 21.4.2.4):
/// MAC PDU type (2) + PDU subtype (1) + fill-bit indication (1). Each MAC-FRAG-UL
/// continuation carries a full SCH/F slot of TM-SDU (no length indication — the
/// terminating MAC-END-UL carries it) so the whole 268-bit block minus this
/// header is TM-SDU.
const MAC_FRAG_UL_HEADER_BITS: usize = 4;

/// Maximum number of full SCH/F slots a single fragmented uplink transfer may
/// reserve (one MAC-ACCESS frag-start plus this many full slots of
/// MAC-FRAG-UL/MAC-END-UL). No realistic MM/CMCE signalling TM-SDU needs more
/// than a couple of full slots; larger transfers are rejected at frag-start so
/// the reserved-slot bookkeeping stays bounded (ETSI TS 100 392-2 cl. 23.5.2).
const MAX_UL_FRAG_SLOTS: usize = 6;

/// TETRA broadcast identity: the 24-bit SSI with all bits set (ETSI TS 100
/// 392-2 addressing). MAC PDUs addressed to it are processed by every MS.
const BROADCAST_SSI: u32 = 0xFF_FFFF;

/// TCH/S type-1 (ACELP) speech frame length in bits (ETSI TS 100 392-2 cl. 8.2
/// / EN 300 395-2): a full traffic slot carries one 274-bit type-1 block, which
/// `errorcontrol::encode_tp` channel-codes to a 432-bit type-5 Normal Uplink
/// Burst payload. Matches the BS downlink `TCH_S_CAP`.
const TCH_S_TYPE1_BITS: usize = 274;

/// Bound on the uplink U-plane jitter buffer (`uplink_audio`). UMAC consumes one
/// frame per granted uplink traffic slot; CC-MS may supply frames at a different
/// cadence (cl. 14.5.1.4). A few frames of slack absorb that jitter; older
/// frames are dropped past this cap so a rate mismatch cannot grow the buffer
/// without bound (cl. 23, transmit scheduling).
const UPLINK_AUDIO_MAX_FRAMES: usize = 4;

/// A MAC block built and queued for uplink transmission, awaiting a valid
/// access opportunity. ETSI TS 100 392-2 cl. 23.5 (MAC access). The random
/// access slot-selection algorithm (cl. 23.5.1.4) that decides *when* to send
/// this, and the actual transmission, are driven by the real-time uplink PHY
/// (Phase 3d).
#[derive(Debug, Clone)]
pub struct PendingUplink {
    /// The full type-1 MAC block (e.g. 92-bit SCH/HU), ready for LMAC encode.
    pub mac_block: BitBuffer,
    pub logical_channel: LogicalChannel,
    pub scrambling_code: u32,
    /// Transmit receipt shared with the LLC acknowledged-mode outbound entry
    /// (ETSI TS 100 392-2 cl. 22.3.2.3). The MS-MAC marks it *transmitted* once
    /// the BS acknowledges the random access (MAC-RESOURCE, cl. 23.5.1.4.8) and
    /// *discarded* if the access procedure is abandoned (cl. 23.5.1.4.9). This
    /// drives the LLC T251/N252 retransmit-and-give-up machinery; without it an
    /// acknowledged-mode uplink whose LLC N(R) ack is withheld by the BS would
    /// wedge the basic link forever. `None` for the frag-start of a fragmented
    /// transfer (the receipt travels with the completing MAC-END-HU instead).
    pub tx_reporter: Option<TxReporter>,
    /// L3-specified PDU priority for the random-access gate (ETSI TS 100 392-2
    /// cl. 23.5.1.4.4): the access attempt is only permitted when this priority
    /// is at least the access code's advertised minimum. `None` → the MAC uses
    /// the access-code minimum (unspecified/LLC-internal traffic always passes,
    /// preserving prior behaviour). Set from the L3 `TMA-UNITDATA-REQ`.
    pub pdu_priority: Option<u8>,
    /// L3 emergency flag (cl. 23.5.1.4.4): an emergency transfer on access code A
    /// bypasses the priority gate and doubles the maximum transmission count.
    /// Always `false` for MM signalling (emergency is CMCE-only); plumbed for a
    /// future CMCE MS.
    pub is_emergency: bool,
}

/// How the remainder of a fragmented uplink transfer is completed, chosen from
/// the remainder size when the frag-start is built (ETSI TS 100 392-2
/// cl. 23.4.2.1.2). The capacity request in the frag-start MAC-ACCESS asks the
/// BS for the matching resource (cl. 23.5.2); the grant it returns is checked
/// against this before the fragment end is emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragEndKind {
    /// MAC-END-HU on a single granted SCH/HU subslot (Control Uplink Burst).
    /// Requested with `Req1Subslot`; the remainder fits ~81 TM-SDU bits.
    MacEndHu,
    /// MAC-END-UL on a granted SCH/F full slot (Normal Uplink Burst). Requested
    /// with `Req1Slot`; the remainder fits ~254 TM-SDU bits.
    MacEndUl,
    /// Multi-slot transfer: `total_slots` full SCH/F slots carrying
    /// `total_slots - 1` MAC-FRAG-UL continuations (264 TM-SDU bits each)
    /// followed by a terminating MAC-END-UL (ETSI TS 100 392-2 cl. 23.4.2.1.2).
    /// Requested with `Req{N}Slots` (cl. 23.5.2); used when the remainder is
    /// larger than one full slot can carry.
    MacFragUl { total_slots: usize },
}

/// The remaining TM-SDU of an uplink transfer that did not fit a single
/// MAC-ACCESS random-access burst and was fragmented (ETSI TS 100 392-2
/// cl. 23.4.2.1.2). The first fragment (a MAC-ACCESS "start of fragmentation"
/// carrying a capacity request) is transmitted by random access; this holds
/// the remainder, sent as a MAC-END-HU (subslot) or MAC-END-UL (full slot)
/// once the BS grants the requested uplink capacity (cl. 23.5.2 basic slot
/// granting).
#[derive(Debug, Clone)]
pub struct UplinkFragment {
    /// TM-SDU bits after the first fragment, carried by the fragment-end PDU.
    pub remainder: BitBuffer,
    pub scrambling_code: u32,
    /// Which fragment-end PDU/channel completes this transfer (chosen from the
    /// remainder size when the frag-start was built) and the capacity the BS
    /// grant must match.
    pub end_kind: FragEndKind,
    /// Transmit receipt for the whole fragmented TM-SDU (see [`PendingUplink::tx_reporter`]).
    /// The receipt travels with the completing fragment: the MS-MAC marks it
    /// *transmitted* when the fragment end is emitted and *discarded* if the
    /// remainder cannot be sent (no grant / mismatched allocation), so the LLC
    /// retransmits the whole transfer.
    pub tx_reporter: Option<TxReporter>,
}

/// An in-flight multi-slot reserved uplink transfer (ETSI TS 100 392-2
/// cl. 23.4.2.1.2 / 23.5.2). When the BS grants N full slots in response to a
/// frag-start capacity request, the remainder is pre-built into N full-slot
/// blocks — `N-1` MAC-FRAG-UL continuations followed by a terminating
/// MAC-END-UL — and this plan drives them out **one block per reserved slot**.
///
/// The MS PHY (`phy_ms`) can only physically transmit at "current downlink slot
/// + 2", so all N blocks cannot be emitted at once: [`UmacMs::drive_reserved_tx`]
/// emits the front block only when its reserved slot's uplink time (`dltime + 2`)
/// is reached, popping `blocks`/`slots` in lockstep. The reserved-slot sequence
/// mirrors the BS `ul_find_grant_opportunity` "next opportunity" stepping
/// (same timeslot every TDMA frame, skipping the mandatory CLCH slot).
#[derive(Debug, Clone)]
struct ReservedTxPlan {
    /// The pre-built full-slot MAC blocks, front = next to transmit. All but the
    /// last are MAC-FRAG-UL; the last is the terminating MAC-END-UL.
    blocks: VecDeque<BitBuffer>,
    /// The reserved uplink slot for each block (same length/order as `blocks`).
    slots: VecDeque<TdmaTime>,
    scrambling_code: u32,
    /// Transmit receipt for the whole fragmented TM-SDU (see
    /// [`PendingUplink::tx_reporter`]); marked *transmitted* only when the final
    /// MAC-END-UL block is emitted, *discarded* if a reserved slot is missed.
    tx_reporter: Option<TxReporter>,
}

/// A complete uplink transfer waiting behind the one currently in flight.
///
/// The MS-MAC can only have a single uplink transfer contending for random
/// access at a time (`pending_uplink`/`pending_fragment`, cl. 23.5.1.4). The
/// LLC, however, may hand down several TM-SDUs in quick succession — e.g. an
/// unacknowledged BL-ACK auto-ack interleaved with an acknowledged group-attach
/// — so extra transfers are held here in FIFO order and promoted into the
/// active slots by [`UmacMs::promote_next_uplink`] once the current one
/// completes. This prevents a later uplink from overwriting an in-flight
/// transfer and orphaning its [`PendingUplink::tx_reporter`], which would leave
/// the acknowledged basic link wedged (no `t_umac_done`, so the LLC T251/N252
/// retransmit-and-give-up never runs).
#[derive(Debug, Clone)]
struct QueuedUplink {
    pending: PendingUplink,
    fragment: Option<UplinkFragment>,
}

/// Upper bound on [`UmacMs::uplink_queue`]. The basic link is effectively
/// stop-and-wait, so only a handful of transfers ever queue (a data PDU plus a
/// few auto-acks). The cap is a defensive guard against pathological growth; on
/// overflow the oldest queued transfer is dropped with its receipt marked
/// discarded so the LLC retransmits it rather than leaking memory.
const MAX_UPLINK_QUEUE: usize = 16;

pub struct UmacMs {
    // config: Option<SharedConfig>,
    dltime: TdmaTime,
    self_component: TetraEntity,
    config: SharedConfig,
    defrag: MsDefrag,

    /// Provided by MLE over TlmbSap, to compute scrambling code, which is passed to lmac
    mcc: Option<u16>,
    /// Provided by MLE over TlmbSap, to compute scrambling code, which is passed to lmac
    mnc: Option<u16>,
    /// Provided by MLE over TlmbSap, to compute scrambling code, which is passed to lmac
    cc: Option<u8>,
    /// Derived from mcc/mnc, and passed to lmac
    scrambling_code: Option<u32>,

    /// MAC block queued for uplink transmission, awaiting an access opportunity
    /// (cl. 23.5). Consumed by the uplink PHY driver (Phase 3d).
    pending_uplink: Option<PendingUplink>,

    /// Uplink transfers waiting behind `pending_uplink` (see [`QueuedUplink`]).
    /// Populated by `rx_tma_prim` when a transfer is already in flight and
    /// drained by `promote_next_uplink` on each downlink slot once the active
    /// transfer completes.
    uplink_queue: VecDeque<QueuedUplink>,

    /// Random access parameters advertised by the serving cell, per access code
    /// (ACCESS-DEFINE cl. 21.4.4.3 + SYSINFO default-A cl. 21.4.4.1).
    access_params: AccessParamStore,

    /// MS-MAC random access state machine (cl. 23.5.1.4). Decides *when* the
    /// queued `pending_uplink` block may be transmitted on an access opportunity.
    random_access: MsRandomAccess,

    /// The remainder of an uplink TM-SDU that was fragmented because it did not
    /// fit a single MAC-ACCESS random-access burst (cl. 23.4.2.1.2). When
    /// `Some`, the first fragment (a MAC-ACCESS start-of-fragmentation carrying
    /// a capacity request) is in `pending_uplink`; the remainder here is sent as
    /// a MAC-END-HU once the BS grants an uplink subslot (cl. 23.5.2).
    pending_fragment: Option<UplinkFragment>,

    /// An in-flight multi-slot reserved uplink transfer (cl. 23.4.2.1.2 /
    /// 23.5.2). When `Some`, the BS has granted the reserved capacity for a
    /// remainder too large for one full slot, and this holds the pre-built
    /// MAC-FRAG-UL/MAC-END-UL blocks plus their reserved uplink slots; one block
    /// is emitted per slot by `drive_reserved_tx` (called each downlink tick).
    /// While it is set no new uplink transfer may start (the in-flight guard),
    /// so its transmit receipt is never orphaned.
    reserved_tx: Option<ReservedTxPlan>,

    /// Runtime downlink address-filter set (cl. 23.4.1.2.1). Seeded from
    /// `[ms].issi` / `[ms].attach_groups` at construction and updated at runtime
    /// via TL-CONFIGURE (from the MLE-IDENTITIES chain, cl. 17.3.2) so that
    /// dynamic group attach/detach changes which downlink traffic the MAC
    /// accepts. The MAC receive filter consults these instead of reading the
    /// static config, so `accept_downlink_address` reflects the live attached set.
    valid_individual_ssi: Option<u32>,
    valid_group_ssis: BTreeSet<u32>,

    /// Last uplink carrier (Hz) derived from the serving cell's SYSINFO duplex
    /// parameters (band + main carrier + duplex spacing resolved through the
    /// programmed duplex table, EN 300 392-2 cl. 18.4.2.2 / cl. 21.4.4). The MS
    /// derives the uplink at camp — not from config — and requests the PHY
    /// retune the transmitter (`TmvTxTuneReq`) only when this value changes, so
    /// the retune is issued once per (re)selected cell rather than on every
    /// SYSINFO broadcast.
    derived_ul_freq: Option<u32>,

    /// Serving cell downlink main carrier number, adopted from SYSINFO
    /// (`main_carrier`, cl. 21.4.4). Held so a received CHANNEL ALLOCATION
    /// element (cl. 21.5.2) can be classified as same-carrier (its
    /// `carrier_num` equals this) versus a different carrier. M2 acts only on
    /// same-carrier allocations; cross-carrier retune is deferred to M3.
    serving_carrier_num: Option<u16>,

    /// Which downlink timeslots (indexed by timeslot 1..4) the serving cell has
    /// assigned to this MS as a traffic channel via a same-carrier CHANNEL
    /// ALLOCATION element (cl. 21.5.2, carried in the MAC-RESOURCE that also
    /// carries the D-SETUP / D-CONNECT / D-TX-GRANTED for our call, cl.
    /// 14.5.1.3). This is the authoritative assigned-timeslot record used to
    /// gate the U-plane TMD relay: decoded speech is only forwarded up on a
    /// timeslot the network actually assigned to us, so bursts on other
    /// (control or other-call) timeslots are dropped at the MAC. The LMAC still
    /// gates the physical decode on the per-slot AACH traffic marker
    /// (cl. 21.4.7.2, `cur_burst.is_traffic`), and CC-MS owns the definitive
    /// U-plane switch gate (is the call actually receiving, cl. 14.5.1.4). The
    /// assigned slot is followed from the element, never hardcoded to the
    /// control timeslot (a same-carrier TCH is on some slot other than the
    /// MCCH's). Cross-carrier / cell-change allocations are deferred to M3.
    assigned_traffic_slots: [bool; 4],

    /// U-plane transmit grant for this MS, set from the MLE-CONFIGURE seam
    /// (`TlmcUPlaneConfigureReq`, cl. 17.3.3 / cl. 14.5.1.4). `true` while CC-MS
    /// holds the transmission grant and the U-plane is switched on, i.e. this MS
    /// is the current talker and may emit uplink TCH/S traffic. The transmit
    /// scheduler (cl. 23) gates uplink traffic emission on this AND on the
    /// assigned uplink slot from `assigned_traffic_slots` (cl. 21.5.2, the sole
    /// slot authority). Cleared when the grant/U-plane is switched off, which
    /// also flushes any buffered uplink audio so a later grant cannot emit stale
    /// frames.
    uplink_tx_granted: bool,

    /// Bounded jitter buffer of uplink U-plane source frames supplied by CC-MS
    /// (the U-plane owner, cl. 14.5.1.4) over the TMD-SAP. UMAC is the transmit
    /// timing authority (cl. 23): it clocks exactly one frame out per granted
    /// uplink traffic slot in `tick_start`, independent of the rate at which
    /// CC-MS pushes frames. Each entry is a 274-bit TCH/S type-1 frame carried
    /// as packed bytes (mirrors the BS `TmdCircuitDataReq` producer convention).
    /// Drop-oldest on overflow so a slow consumer cannot grow it without bound;
    /// on underrun the scheduler emits a silence frame instead.
    uplink_audio: VecDeque<Vec<u8>>,

    /// Queue of associated-signalling MAC blocks (STCH, cl. 21.4.3.3 MAC-DATA)
    /// awaiting transmission by stealing a half-slot of the granted uplink
    /// traffic channel (FACCH/STCH stealing, cl. 23). Populated by `rx_tma_prim`
    /// when L3 requests stealing (`stealing_permission`) while this MS holds an
    /// active uplink traffic grant on an assigned slot — e.g. U-TX-CEASED /
    /// U-TX-DEMAND floor PDUs carried as acknowledged BL-DATA on the
    /// TCH-associated basic link. `drive_uplink_traffic` drains one per granted
    /// traffic slot, placing it in the first (stolen) half of a Normal Uplink
    /// Burst (normal training sequence 2) with the remaining TCH/S speech in the
    /// second half. Each entry carries the LLC transmit receipt so the
    /// acknowledged-mode outbound entry is marked transmitted when the block is
    /// actually emitted (cl. 22.3.2.3).
    pending_stolen_signalling: VecDeque<PendingStolenSignalling>,
}

/// One associated-signalling block queued for FACCH/STCH half-slot stealing on
/// the granted uplink traffic channel (cl. 23).
struct PendingStolenSignalling {
    /// The 124-bit STCH type-1 MAC block (MAC-DATA header + LLC SDU + fill).
    mac_block: BitBuffer,
    /// Serving-cell uplink scrambling code the block must be encoded with.
    scrambling_code: u32,
    /// LLC acknowledged-mode transmit receipt (cl. 22.3.2.3), marked transmitted
    /// when the stolen block is emitted so the basic link does not wedge.
    tx_reporter: Option<TxReporter>,
}

impl UmacMs {
    pub fn new(config: SharedConfig) -> Self {
        // Seed the runtime downlink address-filter set from config (cl.
        // 23.4.1.2.1). This preserves pre-registration behaviour: before any
        // MLE-IDENTITIES update the MS accepts its own ISSI and the configured
        // groups. Runtime group attach/detach later replaces the group set.
        let (valid_individual_ssi, valid_group_ssis) = {
            let cfg = config.config();
            match cfg.ms.as_ref() {
                Some(ms) => (
                    Some(ms.issi),
                    ms.attach_groups.iter().copied().collect::<BTreeSet<u32>>(),
                ),
                None => (None, BTreeSet::new()),
            }
        };
        Self {
            dltime: TdmaTime::default(),
            self_component: TetraEntity::Umac,
            config,
            defrag: MsDefrag::new(),

            mcc: None,
            mnc: None,
            cc: None,
            scrambling_code: None,
            pending_uplink: None,
            uplink_queue: VecDeque::new(),
            access_params: AccessParamStore::new(),
            random_access: MsRandomAccess::new(),
            pending_fragment: None,
            reserved_tx: None,
            valid_individual_ssi,
            valid_group_ssis,
            derived_ul_freq: None,
            serving_carrier_num: None,
            assigned_traffic_slots: [false; 4],
            uplink_tx_granted: false,
            uplink_audio: VecDeque::new(),
            pending_stolen_signalling: VecDeque::new(),
        }
    }

    fn rx_tmv_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tmv_prim");
        match message.msg {
            SapMsgInner::TmvUnitdataInd(_) => {
                self.rx_tmv_unitdata_ind(queue, message);
            }
            _ => {
                panic!();
            }
        }
    }

    pub fn rx_tmv_unitdata_ind(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };
        tracing::trace!("rx_tmv_unitdata_ind: {:?}", prim.logical_channel);

        match prim.logical_channel {
            LogicalChannel::Aach => {
                self.rx_tmv_aach(queue, message);
            }

            LogicalChannel::Bsch => {
                self.rx_tmv_bsch(queue, message);
            }

            LogicalChannel::SchF => {
                // Full slot signalling
                assert!(
                    prim.block_num == PhyBlockNum::Both,
                    "{:?} can't have block_num {:?}",
                    prim.logical_channel,
                    prim.block_num
                );
                self.rx_tmv_sch(queue, message);
            }

            LogicalChannel::Bnch | LogicalChannel::Stch | LogicalChannel::SchHd => {
                // Half slot signalling
                assert!(
                    matches!(prim.block_num, PhyBlockNum::Block1 | PhyBlockNum::Block2),
                    "{:?} can't have block_num {:?}",
                    prim.logical_channel,
                    prim.block_num
                );
                self.rx_tmv_sch(queue, message);
            }
            _ => unreachable!("invalid channel: {:?}", prim.logical_channel),
        }
    }

    /// Receive signalling (SCH, or STCH / BNCH)
    pub fn rx_tmv_sch(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_tmv_sch");

        // Iterate until no more messages left in mac block
        loop {
            // Extract info from inner block
            let SapMsgInner::TmvUnitdataInd(prim) = &message.msg else {
                panic!()
            };
            let Some(bits) = prim.pdu.peek_bits(3) else {
                tracing::warn!("insufficient bits: {}", prim.pdu.dump_bin());
                return;
            };
            let Ok(pdu_type) = MacPduType::try_from(bits >> 1) else {
                tracing::warn!("invalid pdu type: {}", bits >> 1);
                return;
            };
            let orig_start = prim.pdu.get_raw_start();
            let lchan = prim.logical_channel;

            match pdu_type {
                MacPduType::MacResourceMacData => {
                    self.rx_mac_resource(queue, &mut message);
                }
                MacPduType::MacFragMacEnd => {
                    // Also need third bit; designates mac-frag versus mac-end
                    if bits & 1 == 0 {
                        self.rx_mac_frag(queue, &mut message);
                    } else {
                        self.rx_mac_end(queue, &mut message);
                    }
                }
                MacPduType::Broadcast => {
                    self.rx_broadcast(queue, &mut message);
                }
                MacPduType::SuppMacUSignal => {
                    if lchan == LogicalChannel::Stch {
                        // U-SIGNAL since we're on the stealing channel
                        self.rx_usignal(queue, &mut message);
                    } else {
                        self.rx_supp(queue, &mut message);
                    }
                }
            }

            // Check if end of message reached by re-borrowing inner
            // If start was not updated, we also consider it end of message
            // If 16 or more bits remain (len of null pdu), we continue parsing
            if let SapMsgInner::TmvUnitdataInd(prim) = &message.msg {
                if prim.pdu.get_raw_start() != orig_start && prim.pdu.get_len() >= 16 {
                    tracing::trace!(
                        "rx_tmv_unitdata_ind_sch: Remaining {} bits: {:?}",
                        prim.pdu.get_len_remaining(),
                        prim.pdu.dump_bin_full(true)
                    );
                } else {
                    tracing::trace!("rx_tmv_unitdata_ind_sch: End of message reached");
                    break;
                }
            }
        }
    }

    // message pos: start of broadcast frame
    // Will NOT advance pos but pass to underlying function
    fn rx_broadcast(&mut self, queue: &mut MessageQueue, message: &mut SapMsg) {
        tracing::trace!("rx_broadcast");

        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };
        assert!(prim.pdu.peek_bits(2).unwrap() == MacPduType::Broadcast.into_raw()); // MAC PDU type

        let bits = prim.pdu.peek_bits_posoffset(2, 2).unwrap();
        let bcast_type = BroadcastType::try_from(bits).expect("invalid broadcast type");

        match bcast_type {
            BroadcastType::Sysinfo => {
                self.rx_broadcast_sysinfo(queue, message);
            }
            BroadcastType::AccessDefine => {
                self.rx_broadcast_access_define(message);
            }
            _ => {
                panic!();
            }
        }
    }

    // Parses an ACCESS-DEFINE PDU (ETSI TS 100 392-2 cl. 21.4.4.3) and adopts
    // the random access parameters for the access code it defines
    // (cl. 23.5.1.4.1).
    fn rx_broadcast_access_define(&mut self, message: &mut SapMsg) {
        tracing::trace!("rx_broadcast_access_define");
        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };

        let pdu = match AccessDefine::from_bitbuf(&mut prim.pdu) {
            Ok(pdu) => {
                tracing::debug!("<- {}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing AccessDefine: {:?} {}", e, prim.pdu.dump_bin());
                return;
            }
        };

        // An MS on its common control channel ignores ACCESS-DEFINE PDUs marked
        // for an assigned control channel (cl. 23.5.1.4.1). The MS currently
        // only camps on the common control channel.
        if pdu.common_or_assigned_control {
            tracing::trace!("rx_broadcast_access_define: ignoring assigned-control ACCESS-DEFINE");
            return;
        }

        self.access_params.update_access_define(&pdu);
    }

    // Parses the sysinfo pdu
    fn rx_broadcast_sysinfo(&mut self, queue: &mut MessageQueue, message: &mut SapMsg) {
        tracing::trace!("rx_broadcast_sysinfo");
        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };

        // Parse SYSINFO header and optional data
        let pdu = match MacSysinfo::from_bitbuf(&mut prim.pdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing MacSysinfo: {:?} {}", e, prim.pdu.dump_bin());
                return;
            }
        };

        // Adopt the "default definition for access code A" if present
        // (cl. 21.4.4.1 / 23.5.1.4.10). Ignored by the store once a "common"
        // ACCESS-DEFINE for code A has been received.
        if let Some(def) = &pdu.default_access_code {
            self.access_params.update_sysinfo_default_a(def);
        }

        // Record the serving cell's downlink main carrier (cl. 21.4.4) so a
        // received CHANNEL ALLOCATION element (cl. 21.5.2) can be classified as
        // same-carrier vs a different carrier. M2 acts only on same-carrier
        // assignments; cross-carrier retune is deferred to M3.
        self.serving_carrier_num = Some(pdu.main_carrier);

        // Derive the uplink carrier from the cell's broadcast duplex parameters
        // and retune the transmitter if it changed (EN 300 392-2 cl. 18.4.2.2 /
        // cl. 21.4.4). The MS derives its uplink at camp from SYSINFO — not from
        // config — so the TX chain follows whatever cell it is actually on.
        self.maybe_retune_uplink(queue, &pdu);

        // TODO FIXME adopt sysinfo info into global state
        if pdu.hyperframe_number.is_some() && pdu.hyperframe_number.unwrap() != self.dltime.h {
            // Send message to Phy about new hyperframe number
            let mut new_time = self.dltime;
            new_time.h = pdu.hyperframe_number.unwrap();
            let t = TdmaTime {
                t: self.dltime.t,
                f: self.dltime.f,
                m: self.dltime.m,
                h: pdu.hyperframe_number.unwrap(),
            };
            let m = SapMsg {
                sap: Sap::TmvSap,
                src: self.self_component,
                dest: TetraEntity::Lmac,
                msg: SapMsgInner::TmvConfigureReq(TmvConfigureReq {
                    time: Some(t),
                    ..Default::default()
                }),
            };
            tracing::info!("rx_broadcast_sysinfo: Updated TdmaTime: {:?} -> {:?}", self.dltime, new_time);
            queue.push_back(m);
        }

        let tlsdu = BitBuffer::from_bitbuffer_pos(&prim.pdu);
        let m = SapMsg {
            sap: Sap::TlmbSap,
            src: TetraEntity::Umac,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::TlmbSysinfoInd(TlmbSysinfoInd {
                endpoint_id: 0,
                tl_sdu: tlsdu,
                mac_broadcast_info: None,
            }),
        };

        queue.push_back(m);
    }

    /// Derive the uplink carrier from a decoded SYSINFO and, if it differs from
    /// the last derived value, request the PHY retune the transmitter
    /// (`TmvTxTuneReq` -> LMAC -> PHY).
    ///
    /// The uplink is resolved exactly as the codeplug math would (EN 300 392-2
    /// cl. 18.4.2.2 broadcast parameters / cl. 21.4.4 SYSINFO): the downlink is
    /// `100 MHz * band + 25 kHz * main_carrier + freq_offset`, and the duplex
    /// spacing is taken from the programmed [`DuplexTable`] for the broadcast
    /// duplex-spacing index (an operator override, e.g. a non-standard split,
    /// wins over the ETSI default). `reverse_operation` selects UL below/above
    /// DL. Deriving here — rather than seeding from config — means the MS always
    /// transmits on the uplink paired with the cell it actually camped on.
    fn maybe_retune_uplink(&mut self, queue: &mut MessageQueue, pdu: &MacSysinfo) {
        // Compute the uplink under the config guard, then drop the guard before
        // mutating self / queueing.
        let ul = {
            let cfg = self.config.config();
            if cfg.stack_mode != StackMode::Ms {
                return;
            }
            // Guard the value ranges the FreqInfo constructor asserts on, so a
            // malformed SYSINFO logs and is ignored instead of panicking.
            if pdu.freq_band > 8 || pdu.main_carrier >= 4000 {
                tracing::warn!(
                    "SYSINFO out-of-range RF params (band {}, carrier {}); not deriving uplink",
                    pdu.freq_band,
                    pdu.main_carrier
                );
                return;
            }
            let freq_offset_hz = match FreqInfo::freq_offset_id_to_hz(pdu.freq_offset_index) {
                Some(v) => v,
                None => {
                    tracing::warn!("SYSINFO invalid freq offset index {}", pdu.freq_offset_index);
                    return;
                }
            };
            match FreqInfo::from_components_with_table(
                pdu.freq_band,
                pdu.main_carrier,
                freq_offset_hz,
                pdu.reverse_operation,
                pdu.duplex_spacing,
                None,
                &cfg.duplex_table,
            ) {
                Ok(freq_info) => freq_info.get_freqs().1,
                Err(e) => {
                    tracing::warn!("Cannot derive uplink from SYSINFO: {}", e);
                    return;
                }
            }
        };

        if self.derived_ul_freq == Some(ul) {
            // Unchanged since the last SYSINFO — nothing to retune.
            return;
        }
        self.derived_ul_freq = Some(ul);
        tracing::info!(
            "UMAC: derived uplink {} Hz from SYSINFO (band {}, carrier {}, duplex idx {}, reverse {}); retuning TX",
            ul,
            pdu.freq_band,
            pdu.main_carrier,
            pdu.duplex_spacing,
            pdu.reverse_operation
        );
        queue.push_back(SapMsg {
            sap: Sap::TmvSap,
            src: self.self_component,
            dest: TetraEntity::Lmac,
            msg: SapMsgInner::TmvTxTuneReq(TmvTxTuneReq { carrier_hz: ul }),
        });
    }

    fn rx_mac_resource(&mut self, queue: &mut MessageQueue, message: &mut SapMsg) {
        tracing::trace!("rx_mac_resource");
        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };
        assert!(prim.pdu.get_pos() == 0); // We should be at the start of the MAC PDU

        // Parse header and optional ChanAlloc
        let pdu = match MacResource::from_bitbuf(&mut prim.pdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing MacResource: {:?} {}", e, prim.pdu.dump_bin());
                return;
            }
        };

        if pdu.encryption_mode > 0 {
            unimplemented_log!("rx_mac_resource: Encryption mode > 0, not implemented");
        }

        // Feed the random access response detector (cl. 23.5.1.4.8): a
        // MAC-RESOURCE addressed to our ISSI with the random access flag set
        // acknowledges the request we sent, completing the access procedure.
        if self.random_access.is_active() {
            let addr_matches = pdu.addr.as_ref().is_some_and(|a| a.ssi == self.own_issi());
            if let Some(RaAction::Succeeded) =
                self.random_access.on_mac_resource(addr_matches, pdu.random_access_flag)
            {
                tracing::info!("random access: request acknowledged by BS (MAC-RESOURCE)");
                let completed = self.pending_uplink.take();

                // If the acknowledged request was the start of a fragmented
                // uplink transfer (cl. 23.4.2.1.2), this MAC-RESOURCE also
                // carries the subslot grant for the MAC-END-HU remainder
                // (cl. 23.5.2 basic slot granting). Emit it now; the transmit
                // receipt (held on the fragment) is marked there.
                if self.pending_fragment.is_some() {
                    self.emit_fragment_end(queue, pdu.slot_granting_element.as_ref());
                } else if let Some(pending) = completed {
                    // Unfragmented: the whole TM-SDU is now delivered to the
                    // BS-MAC. Mark the LLC transmit receipt so the acknowledged
                    // basic link can start its ack-wait / retransmit timer
                    // (cl. 22.3.2.3); panic-safe against an already-marked
                    // receipt (a re-presented retransmission).
                    if let Some(tx_reporter) = pending.tx_reporter {
                        tx_reporter.try_mark_transmitted();
                    }
                }
            }
        }

        // Compute len
        let mut pdu_len_bits = {
            match pdu.length_ind {
                0b000000 => {
                    // Null PDU (length indication 00000 2, cl. 21.4.3.1 /
                    // Table 21.55): the MAC PDU carries no TM-SDU, so its length
                    // is the MAC header only. Handled as downlink filler below
                    // (dropped, not passed to LLC).
                    pdu.compute_header_len()
                }
                0b000001..0b111010 => {
                    // tracing::trace!("rx_mac_resource: length_ind {}", pdu.length_ind);
                    pdu.length_ind as usize * 8
                }
                0b111110 => {
                    // Second half slot stolen in STCH (cl. 9.4.4.3.2 / Table
                    // 21.55): this MAC PDU occupies the rest of the first stolen
                    // half-slot and the second half-slot is also stolen for
                    // associated signalling. The block1 PDU is still delivered to
                    // LLC below; the LMAC is told to route Block2 as STCH (not
                    // TCH/S) in the delivery branch further down.
                    prim.pdu.get_len()
                }
                0b111111 => {
                    // Start of fragmentation
                    // tracing::trace!("rx_mac_resource: frag start length_ind {}", pdu.length_ind);
                    prim.pdu.get_len()
                }
                _ => panic!("rx_mac_resource: Invalid length_ind {}", pdu.length_ind),
            }
        };

        if pdu_len_bits > prim.pdu.get_len() {
            // TODO FIXME: I sometimes encounter len = 0b100010 = 32
            // This does not fit, since it translates to 272 bits while it comes in a 268 bit slot
            // We'll correct for that by simply cropping to the end... But this is strange
            tracing::warn!(
                "rx_mac_resource: Strange length_ind {} in MAC resource, truncating from {} to {}",
                pdu.length_ind,
                pdu_len_bits,
                prim.pdu.get_len()
            );
            pdu_len_bits = prim.pdu.get_len();
        }

        // Strip fill bits. Maintain original end to allow for later parsing of a second mac block
        tracing::trace!("rx_mac_resource: {}", prim.pdu.dump_bin_full(true));
        let num_fill_bits = {
            if pdu.fill_bits {
                fillbits::removal::get_num_fill_bits(&prim.pdu, pdu_len_bits, pdu.is_null_pdu())
            } else {
                0
            }
        };
        pdu_len_bits -= num_fill_bits;
        let orig_end = prim.pdu.get_raw_end();
        prim.pdu.set_raw_end(prim.pdu.get_raw_start() + pdu_len_bits);
        tracing::trace!(
            "rx_mac_resource: pdu: {} sdu: {} fb: {}: {}",
            pdu_len_bits,
            prim.pdu.get_len_remaining(),
            num_fill_bits,
            prim.pdu.dump_bin_full(true)
        );

        if pdu.addr.is_none() {
            // TODO not sure if there is scenarios in which we want to pass a null pdu to the LLC
            // tracing::warn!("rx_mac_resource: Null PDU not passed to LLC");
            return;
        }

        // Decrypt if needed
        if pdu.encryption_mode > 0 {
            unimplemented_log!("rx_mac_resource: Encryption mode > 0");
            return;
            // TODO:
            // Check if key available
            // generate keystream
            // apply keystream to data
            // re-decode chanalloc
            // continue
        }

        tracing::debug!("rx_mac_resource: {}", prim.pdu.dump_bin_full(true));

        // MS receive filter (ETSI TS 100 392-2 clause 23 addressing): ignore
        // MAC-RESOURCE PDUs addressed to other subscribers/groups. Applied in
        // MS mode only; monitor mode observes all traffic. `pdu.addr` is
        // guaranteed `Some` here (null PDU returned above).
        if !self.accept_downlink_address(&pdu.addr.unwrap()) {
            tracing::debug!(
                "rx_mac_resource: dropping PDU addressed to {} (not this MS)",
                pdu.addr.unwrap()
            );
        } else {
            // Act on a CHANNEL ALLOCATION element addressed to us (cl. 21.5.2).
            // The BS carries it in the MAC-RESOURCE that also carries the
            // D-SETUP / D-CONNECT / D-TX-GRANTED for our call (cl. 14.5.1.3), so
            // this is where the MS learns which timeslot its traffic channel is
            // on. Done before the LLC hand-off so the assigned-slot record is in
            // place when the U-plane later switches on. Same-carrier only in M2.
            if let Some(ca) = &pdu.chan_alloc_element {
                self.act_on_channel_allocation(ca);
            }

            if pdu.length_ind == 0b111111 {
                // Fragmentation start, add to defragmenter
                self.defrag.insert_first(&mut prim.pdu, self.dltime, pdu.addr.unwrap(), None);
            } else {
                if pdu.length_ind == 0b111110 {
                    // Second half slot stolen in STCH (cl. 9.4.4.3.2): tell LMAC
                    // to route this slot's Block2 as STCH signalling rather than
                    // TCH/S speech, so a group-addressed D-TX GRANTED carried in
                    // the second stolen half (e.g. naming the current group-call
                    // talker) reaches CMCE. Must be signalled before Block2 is
                    // classified; the block1 PDU below is delivered normally.
                    self.signal_lmac_second_half_stolen(queue);
                }
                // Pass directly to LLC
                let sdu = {
                    if pdu.length_ind == 0 {
                        None // Null PDU
                    } else if prim.pdu.get_len_remaining() == 0 {
                        None // No more data in this block
                    } else {
                        // TODO FIXME should not copy here but take ownership
                        // Copy inner part, without MAC header or fill bits
                        Some(BitBuffer::from_bitbuffer_pos(&prim.pdu))
                    }
                };
                // tracing::debug!("rx_mac_resource: sdu: {:?}", sdu.as_ref().unwrap().dump_bin_full(true));

                if sdu.is_some() {
                    // We have an SDU for the LLC, deliver it.
                    let m = SapMsg {
                        sap: Sap::TmaSap,
                        src: TetraEntity::Umac,
                        dest: TetraEntity::Llc,
                        msg: SapMsgInner::TmaUnitdataInd(TmaUnitdataInd {
                            pdu: sdu,
                            main_address: pdu.addr.unwrap(),
                            scrambling_code: prim.scrambling_code,
                            endpoint_id: 0,        // TODO FIXME
                            new_endpoint_id: None, // TODO FIXME
                            css_endpoint_id: None, // TODO FIXME
                            air_interface_encryption: pdu.encryption_mode as Todo,
                            chan_change_response_req: false,
                            chan_change_handle: None,
                            chan_info: None,
                        }),
                    };
                    queue.push_back(m);
                } else {
                    // Either this is a null pdu or we are at the end of the block
                    // For now, we don't deliver this. However, important data may need to be signalled upwards
                    tracing::info!("rx_mac_resource: empty PDU not passed to LLC");
                }
            }
        }

        // Since this is not a null pdu, more MAC PDUs may follow
        // This allows parent function to continue parsing
        prim.pdu.set_raw_end(orig_end);
        prim.pdu.set_raw_pos(prim.pdu.get_raw_start() + pdu_len_bits + num_fill_bits);
        prim.pdu.set_raw_start(prim.pdu.get_raw_pos());
    }

    /// Tell LMAC that this slot's Block2 is also stolen for associated
    /// signalling (STCH, not TCH/S speech), so it is decoded on the control-plane
    /// chain rather than the speech path (cl. 9.4.4.3.2). Immediate priority so
    /// LMAC applies it before classifying the Block2 burst. Mirrors
    /// `UmacBs::signal_lmac_second_half_stolen`.
    fn signal_lmac_second_half_stolen(&mut self, queue: &mut MessageQueue) {
        let m = SapMsg {
            sap: Sap::TmvSap,
            src: TetraEntity::Umac,
            dest: TetraEntity::Lmac,
            msg: SapMsgInner::TmvConfigureReq(TmvConfigureReq {
                blk2_stolen: Some(true),
                ..Default::default()
            }),
        };
        queue.push_prio(m, MessagePrio::Immediate);
    }

    fn rx_mac_frag(&mut self, _queue: &mut MessageQueue, message: &mut SapMsg) {
        tracing::trace!("rx_mac_frag");
        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };
        assert!(prim.pdu.get_pos() == 0); // We should be at the start of the MAC PDU

        // Parse header and optional ChanAlloc
        let pdu = match MacFragDl::from_bitbuf(&mut prim.pdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing MacFragDl: {:?} {}", e, prim.pdu.dump_bin());
                return;
            }
        };

        // Strip fill bits. This message is known to fill the slot.
        let mut pdu_len_bits = prim.pdu.get_len();
        let num_fill_bits = {
            if pdu.fill_bits {
                fillbits::removal::get_num_fill_bits(&prim.pdu, pdu_len_bits, false)
            } else {
                0
            }
        };
        pdu_len_bits -= num_fill_bits;
        prim.pdu.set_raw_end(prim.pdu.get_raw_start() + pdu_len_bits);
        tracing::debug!("rx_mac_frag: pdu_len_bits: {} fill_bits: {}", pdu_len_bits, num_fill_bits);

        // Decrypt if needed
        if let Some(_aie_info) = self.defrag.buffers[(self.dltime.t - 1) as usize].aie_info {
            // TODO FIXME implement
            unimplemented_log!("rx_mac_frag: Encryption not supported");
            return;
        }

        // Insert into defragmenter
        self.defrag.insert_next(&mut prim.pdu, self.dltime);
    }

    fn rx_mac_end(&mut self, queue: &mut MessageQueue, message: &mut SapMsg) {
        tracing::trace!("rx_mac_end");
        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };
        assert!(prim.pdu.get_pos() == 0); // We should be at the start of the MAC PDU

        // Parse header and optional ChanAlloc
        let pdu = match MacEndDl::from_bitbuf(&mut prim.pdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing MacEndDl: {:?} {}", e, prim.pdu.dump_bin());
                return;
            }
        };

        // Compute len
        assert!(pdu.length_ind != 0); // Reserved
        let mut pdu_len_bits = pdu.length_ind as usize * 8;

        // Strip fill bits. Maintain original end to allow for later parsing of a second mac block
        let num_fill_bits = {
            if pdu.fill_bits {
                fillbits::removal::get_num_fill_bits(&prim.pdu, pdu_len_bits, false)
            } else {
                0
            }
        };
        pdu_len_bits -= num_fill_bits;
        let orig_end = prim.pdu.get_raw_end();
        prim.pdu.set_raw_end(prim.pdu.get_raw_start() + pdu_len_bits);
        tracing::debug!("rx_mac_end: pdu_len_bits: {} fill_bits: {}", pdu_len_bits, num_fill_bits);

        // Decrypt if needed
        if let Some(_aie_info) = self.defrag.buffers[(self.dltime.t - 1) as usize].aie_info {
            // TODO FIXME implement
            unimplemented!("rx_mac_end: Encryption not supported");
            // TODO FIXME Also re-parse chanalloc
        }

        // Insert into defragmenter
        self.defrag.insert_last(&mut prim.pdu, self.dltime);

        // Fetch finalized block
        let defragbuf = self.defrag.take_defragged_buf(self.dltime);
        let Some(defragbuf) = defragbuf else {
            tracing::warn!("rx_mac_end: could not obtain defragged buf");
            return;
        };

        // Pass block directly to LLC
        tracing::debug!("rx_mac_end: sdu: {:?}", defragbuf.buffer.dump_bin());

        // MS receive filter (ETSI TS 100 392-2 clause 23 addressing): drop a
        // reassembled message addressed to another subscriber/group. The first
        // fragment is normally already filtered in rx_mac_resource, so this is
        // a defensive backstop. Advance the parse position so any following
        // MAC PDU in the block is still parsed.
        if !self.accept_downlink_address(&defragbuf.addr) {
            tracing::debug!(
                "rx_mac_end: dropping reassembled PDU addressed to {} (not this MS)",
                defragbuf.addr
            );
            prim.pdu.set_raw_end(orig_end);
            prim.pdu.set_raw_pos(prim.pdu.get_raw_start() + pdu_len_bits + num_fill_bits);
            prim.pdu.set_raw_start(prim.pdu.get_raw_pos());
            return;
        }

        let m = SapMsg {
            sap: Sap::TmaSap,
            src: TetraEntity::Umac,
            dest: TetraEntity::Llc,
            msg: SapMsgInner::TmaUnitdataInd(TmaUnitdataInd {
                pdu: Some(defragbuf.buffer),
                main_address: defragbuf.addr,
                scrambling_code: prim.scrambling_code,
                endpoint_id: 0,              // TODO FIXME
                new_endpoint_id: None,       // TODO FIXME
                css_endpoint_id: None,       // TODO FIXME
                air_interface_encryption: 0, // TODO FIXME implement
                chan_change_response_req: false,
                chan_change_handle: None,
                chan_info: None,
            }),
        };
        queue.push_back(m);

        // Since this is not a null pdu, more MAC PDUs may follow
        // This allows parent function to continue parsing
        prim.pdu.set_raw_end(orig_end);
        prim.pdu.set_raw_pos(prim.pdu.get_raw_start() + pdu_len_bits + num_fill_bits);
        prim.pdu.set_raw_start(prim.pdu.get_raw_pos());
    }

    fn rx_usignal(&self, _queue: &mut MessageQueue, message: &mut SapMsg) {
        tracing::trace!("rx_usignal");
        let SapMsgInner::TmvUnitdataInd(_prim) = &mut message.msg else {
            panic!()
        };
        unimplemented!("rx_usignal");
    }

    fn rx_supp(&self, _queue: &mut MessageQueue, message: &mut SapMsg) {
        tracing::trace!("rx_supp");

        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };
        // Check we're indeed on the right channel (Clause 21.4.1 Table 21.48)
        assert!(prim.logical_channel != LogicalChannel::Stch && prim.logical_channel != LogicalChannel::SchHd);
        unimplemented!("rx_supp");
    }

    pub fn rx_tmv_aach(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_tmv_aach");

        // TODO FIXME, more extensively store and process AACH state in both LMAC and UMAC
        // Then we send a msg down only if a change is needed, like we do for the scrambling code

        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };

        // Keep the parsed ACCESS-ASSIGN so we can drive the random access state
        // machine (cl. 23.5.1.4) against this slot's uplink access rights.
        let mut access_assign: Option<AccessAssign> = None;
        let is_traffic = if self.dltime.f != 18 {
            let pdu = match AccessAssign::from_bitbuf(&mut prim.pdu) {
                Ok(pdu) => {
                    tracing::debug!("<- {:?}", pdu);
                    pdu
                }
                Err(e) => {
                    tracing::warn!("Failed parsing AccessAssign: {:?} {}", e, prim.pdu.dump_bin());
                    return;
                }
            };

            let traffic = pdu.dl_usage.is_traffic();
            access_assign = Some(pdu);
            traffic
        } else {
            // Frame 18 carries AccessAssignFr18 (no per-subslot access field, cl.
            // 21.4.7.3), so it never designates a random access opportunity here.
            let _pdu = match AccessAssignFr18::from_bitbuf(&mut prim.pdu) {
                Ok(pdu) => {
                    tracing::debug!("<- {:?}", pdu);
                    pdu
                }
                Err(e) => {
                    tracing::warn!("Failed parsing AccessAssignFr18: {:?} {}", e, prim.pdu.dump_bin());
                    return;
                }
            };

            false
        };

        let m = SapMsg {
            sap: Sap::TmvSap,
            src: TetraEntity::Umac,
            dest: TetraEntity::Lmac,
            msg: SapMsgInner::TmvConfigureReq(TmvConfigureReq {
                is_traffic: Some(is_traffic),
                ..Default::default()
            }),
        };
        // This message needs to be processed NOW since it affects the other blocks in this timeslot
        queue.push_prio(m, MessagePrio::Immediate);

        // Drive the MS random access state machine against this slot's ACCESS-ASSIGN.
        if let Some(aa) = access_assign {
            self.drive_random_access(queue, &aa);
        }
    }

    /// Advance the random access state machine (cl. 23.5.1.4) for one downlink
    /// slot's ACCESS-ASSIGN and act on the resulting decision. No-op unless a
    /// random access attempt is currently in progress.
    fn drive_random_access(&mut self, queue: &mut MessageQueue, aa: &AccessAssign) {
        // If the previously active uplink transfer has completed (acknowledged
        // or abandoned), pull the next queued transfer into the active slots so
        // it can contend for access. Done first, every slot, so a transfer that
        // was held behind another (cl. 23.5.1.4 permits only one at a time) is
        // never starved.
        self.promote_next_uplink();

        // Access code A is the default code available to all MSs (SYSINFO
        // default-A, cl. 21.4.4.1). Without advertised parameters we cannot
        // legally access, so keep any queued uplink waiting until the BS
        // broadcasts them (they arrive shortly after camping, via SYSINFO
        // default-A or ACCESS-DEFINE).
        let code = AccessCode::A;
        let Some(params) = self.access_params.params_for(code).cloned() else {
            return;
        };

        // Start the random access procedure for a queued uplink block that has
        // not been initiated yet (cl. 23.5.1.4). Initiation is deferred here
        // from `rx_tma_prim` so an uplink queued before the access parameters
        // were advertised is transmitted as soon as they arrive, rather than
        // dropped. No-op when nothing is queued and no attempt is in progress.
        if !self.random_access.is_active() {
            if self.pending_uplink.is_none() {
                return;
            }
            // The L3-specified PDU priority + emergency flag feed the random-
            // access permission gate (cl. 23.5.1.4.4). They travel on the queued
            // `PendingUplink` (set from the TMA-UNITDATA-REQ in `rx_tma_prim`).
            // `None` → use the access code's minimum so the gate passes
            // (unspecified/LLC-internal traffic, preserving prior behaviour).
            let pu = self
                .pending_uplink
                .as_ref()
                .expect("pending_uplink checked Some above");
            let pdu_prio = pu.pdu_priority.unwrap_or(params.min_pdu_prio);
            let is_emergency = pu.is_emergency;
            if let Err(e) = self
                .random_access
                .initiate(self.dltime, code, &params, pdu_prio, is_emergency)
            {
                tracing::warn!("drive_random_access: random access not initiated: {:?}; dropping uplink SDU", e);
                self.pending_uplink = None;
                return;
            }
        }

        // The ACCESS-ASSIGN on the AACH designates the access rights of the
        // uplink subslots of the slot two timeslots later (cl. 23.5.1.4.2). We
        // are camped on the common control channel, where the per-slot AACH
        // designation is authoritative, so any slot carrying a valid
        // ACCESS-ASSIGN is treated as a potential opportunity (ul_slot_valid).
        let assign = interpret_access_assign(aa, true);
        let mut rng = ThreadRaRng;
        let action =
            self.random_access
                .poll_downlink_slot(self.dltime, &assign, true, &params, &mut rng);

        match action {
            Some(RaAction::Transmit { ul_time, subslot }) => {
                self.emit_uplink(queue, ul_time, subslot);
            }
            Some(RaAction::Failed(f)) => {
                tracing::warn!("random access abandoned: {:?}", f);
                // The MS-MAC exhausted its access attempts without the BS
                // acknowledging (cl. 23.5.1.4.9). Signal the LLC that the MAC
                // could not deliver this TM-SDU so the acknowledged basic link
                // retransmits it (cl. 22.3.2.3); if a fragmented transfer, the
                // receipt lives on the fragment.
                if let Some(pending) = self.pending_uplink.take() {
                    if let Some(tx_reporter) = pending.tx_reporter {
                        tx_reporter.try_mark_discarded();
                    }
                }
                if let Some(fragment) = self.pending_fragment.take() {
                    if let Some(tx_reporter) = fragment.tx_reporter {
                        tx_reporter.try_mark_discarded();
                    }
                }
            }
            Some(RaAction::Succeeded) | None => {}
        }
    }

    /// Emit the queued uplink MAC block to LMAC for transmission on the granted
    /// uplink slot. The block is kept in `pending_uplink` so it can be
    /// retransmitted if the random access attempt has to retry (cl. 23.5.1.4.7).
    fn emit_uplink(&mut self, queue: &mut MessageQueue, ul_time: TdmaTime, subslot: Subslot) {
        let Some(pending) = self.pending_uplink.as_ref() else {
            tracing::warn!("random access requested transmit but no pending uplink block");
            return;
        };

        tracing::info!(
            "random access: transmitting MAC-ACCESS at UL {:?} subslot {:?}",
            ul_time,
            subslot
        );

        let blk = TmvUnitdataReq {
            mac_block: pending.mac_block.clone(),
            logical_channel: pending.logical_channel,
            scrambling_code: pending.scrambling_code,
        };

        // NOTE: the chosen subslot is not yet conveyed to LMAC/PHY (TmvUnitdataReqSlot
        // has slot-level `ts` only). Phase 3d adds subslot keying in PhyMs.
        let m = SapMsg {
            sap: Sap::TmvSap,
            src: self.self_component,
            dest: TetraEntity::Lmac,
            msg: SapMsgInner::TmvUnitdataReq(TmvUnitdataReqSlot {
                ts: ul_time,
                ul_phy_chan: PhysicalChannel::Cp,
                blk1: Some(blk),
                blk2: None,
                bbk: None,
                // Random-access contention burst: a later occurrence of the
                // same opportunity is equivalent, so the PHY may frame-advance
                // it to stay ahead of the TX frontier.
                reserved_access: false,
            }),
        };
        queue.push_back(m);
    }

    /// Mark a fragmented transfer's transmit receipt discarded so the LLC
    /// acknowledged basic link retransmits the whole SDU (cl. 22.3.2.3). Used on
    /// every path where the fragment end cannot be emitted.
    fn discard_fragment(frag: &UplinkFragment) {
        if let Some(tx_reporter) = frag.tx_reporter.as_ref() {
            tx_reporter.try_mark_discarded();
        }
    }

    /// Emit the fragment-end PDU that completes a fragmented uplink transfer, on
    /// the capacity the base station granted in `grant` (ETSI TS 100 392-2
    /// cl. 23.5.2 basic slot granting; cl. 23.4.2.1.2 uplink fragmentation
    /// form i). This is a **reserved-access** transmission (the BS reserved the
    /// slot in response to our frag-start capacity request), so it does not
    /// contend via the random access algorithm. Consumes `pending_fragment`.
    ///
    /// The fragment-end PDU and channel are chosen by the remainder size at
    /// frag-start time ([`FragEndKind`]) and must match the grant:
    /// - `Req1Subslot` -> `FirstSubslotGranted` -> MAC-END-HU on SCH/HU (Control
    ///   Uplink Burst) — remainder up to ~81 TM-SDU bits;
    /// - `Req1Slot` -> `Grant1Slot` -> MAC-END-UL on SCH/F (Normal Uplink Burst)
    ///   — remainder up to ~254 TM-SDU bits;
    /// - `Req{N}Slots` -> `Grant{N}Slots` -> `N-1` MAC-FRAG-UL continuations plus
    ///   a terminating MAC-END-UL across N reserved SCH/F full slots (true
    ///   multi-slot fragmentation) — driven out one block per slot by
    ///   [`Self::drive_reserved_tx`].
    ///
    /// Only the "next opportunity" granting delay (cl. 23.5.2.2.2) is handled;
    /// any other capacity allocation / granting delay, or a grant that does not
    /// match the requested [`FragEndKind`], is logged and the remainder dropped
    /// (the MM layer retransmits the whole demand). For "capacity allocation at
    /// next opportunity" the reserved slot is the same-numbered uplink timeslot
    /// in the same TDMA frame as the downlink slot carrying the grant; since the
    /// uplink frame is a fixed 2-timeslot delay from the downlink (cl. 9.3.9
    /// Frame alignment), that is `dltime + 2`.
    fn emit_fragment_end(&mut self, queue: &mut MessageQueue, grant: Option<&BasicSlotgrant>) {
        let Some(mut frag) = self.pending_fragment.take() else {
            return;
        };
        let rem_len = frag.remainder.get_len();

        let Some(grant) = grant else {
            tracing::warn!(
                "uplink fragmentation: frag-start acknowledged but MAC-RESOURCE carried no slot grant; \
                 cannot send fragment end (LLC/MM will retransmit)"
            );
            Self::discard_fragment(&frag);
            return;
        };

        // Only the "next opportunity" granting delay is used for a reserved
        // fragment transfer (cl. 23.5.2.2.2).
        match grant.granting_delay {
            BasicSlotgrantGrantingDelay::CapAllocAtNextOpportunity => {}
            other => {
                unimplemented_log!(
                    "uplink fragmentation: unsupported granting delay {:?}; dropping fragment end",
                    other
                );
                Self::discard_fragment(&frag);
                return;
            }
        }

        // Multi-slot transfer (cl. 23.4.2.1.2): build the (N-1) MAC-FRAG-UL
        // continuations + terminating MAC-END-UL and set up a reserved
        // transmission plan driven one block per reserved slot. The PHY can only
        // transmit at `dltime + 2`, so the blocks cannot all be emitted at once.
        if let FragEndKind::MacFragUl { total_slots } = frag.end_kind {
            // The grant must reserve at least the N full slots we requested
            // (cl. 23.5.2). Subslot grants (which have no whole-slot count) never
            // match a multi-slot request.
            let granted_slots = match grant.capacity_allocation {
                BasicSlotgrantCapAlloc::FirstSubslotGranted
                | BasicSlotgrantCapAlloc::SecondSubslotGranted => 0,
                other => other.to_req_slotcount(),
            };
            if granted_slots < total_slots {
                unimplemented_log!(
                    "uplink fragmentation: grant {:?} ({} slot(s)) smaller than requested {} slots; \
                     dropping fragment end (LLC/MM will retransmit)",
                    grant.capacity_allocation,
                    granted_slots,
                    total_slots
                );
                Self::discard_fragment(&frag);
                return;
            }
            let Some(blocks) = Self::build_ul_frag_blocks(&mut frag.remainder, total_slots) else {
                tracing::warn!(
                    "uplink fragmentation: could not split {}-bit remainder into {} full slots; \
                     dropping (LLC/MM will retransmit)",
                    rem_len,
                    total_slots
                );
                Self::discard_fragment(&frag);
                return;
            };
            // First reserved slot = dltime + 2 (cl. 9.3.9 / 23.5.2.2.2); the rest
            // step one TDMA frame at a time, skipping the mandatory CLCH slot,
            // mirroring the BS grant-opportunity walk.
            let first = self.dltime.add_timeslots(2);
            let slots = Self::reserved_slot_sequence(first, total_slots);
            debug_assert_eq!(blocks.len(), slots.len());
            tracing::info!(
                "uplink fragmentation: BS granted {:?}; scheduling {}-block multi-slot transfer \
                 ({} SDU bits) starting UL {:?}",
                grant.capacity_allocation,
                total_slots,
                rem_len,
                first
            );
            self.reserved_tx = Some(ReservedTxPlan {
                blocks: blocks.into_iter().collect(),
                slots,
                scrambling_code: frag.scrambling_code,
                tx_reporter: frag.tx_reporter.take(),
            });
            // Emit the first block now if its reserved slot is this uplink slot
            // (dltime + 2). If dltime + 2 was a CLCH slot, slot[0] is later and
            // this correctly does nothing until that slot arrives.
            self.drive_reserved_tx(queue);
            return;
        }

        // The grant's capacity allocation must match the capacity we requested
        // in the frag-start (cl. 23.5.2): a subslot for a MAC-END-HU, a full
        // slot for a MAC-END-UL. Build the matching fragment-end block on its
        // logical channel; the LMAC turns SCH/HU -> Control Uplink Burst and
        // SCH/F -> Normal Uplink Burst (cl. 9.4.4.2).
        let (block, logical_channel) = match (frag.end_kind, grant.capacity_allocation) {
            (FragEndKind::MacEndHu, BasicSlotgrantCapAlloc::FirstSubslotGranted) => {
                match Self::build_mac_end_hu_block(&mut frag.remainder) {
                    Some(block) => (block, LogicalChannel::SchHu),
                    None => {
                        tracing::warn!(
                            "uplink fragmentation: {}-bit remainder too large for a single MAC-END-HU; \
                             dropping (LLC/MM will retransmit)",
                            rem_len
                        );
                        Self::discard_fragment(&frag);
                        return;
                    }
                }
            }
            (FragEndKind::MacEndUl, BasicSlotgrantCapAlloc::Grant1Slot) => {
                match Self::build_mac_end_ul_block(&mut frag.remainder) {
                    Some(block) => (block, LogicalChannel::SchF),
                    None => {
                        tracing::warn!(
                            "uplink fragmentation: {}-bit remainder too large for a single MAC-END-UL; \
                             multi-slot fragmentation not implemented, dropping (LLC/MM will retransmit)",
                            rem_len
                        );
                        Self::discard_fragment(&frag);
                        return;
                    }
                }
            }
            (kind, other) => {
                unimplemented_log!(
                    "uplink fragmentation: grant capacity {:?} does not match requested {:?}; \
                     dropping fragment end",
                    other,
                    kind
                );
                Self::discard_fragment(&frag);
                return;
            }
        };

        // Granting delay "capacity allocation at next opportunity"
        // (cl. 23.5.2.2.2) => the same-numbered uplink timeslot in the same TDMA
        // frame as the downlink slot carrying the grant. The uplink frame is a
        // fixed 2-timeslot delay from the downlink (cl. 9.3.9), so that is
        // dltime + 2 -- the same offset the random access path uses.
        let ul_time = self.dltime.add_timeslots(2);
        tracing::info!(
            "uplink fragmentation: BS granted {:?}; transmitting {:?} ({} SDU bits) at UL {:?} on {:?}",
            grant.capacity_allocation,
            frag.end_kind,
            rem_len,
            ul_time,
            logical_channel
        );

        // NOTE: this fragment-end burst is a reserved-access transmission that
        // must land in the exact granted uplink slot for the BS's per-slot
        // ownership check (cl. 23.5.2) to accept it -- a burst outside the
        // reserved slot is rejected ("MAC-END-* for unassigned block"). TETRA
        // has no timing-advance procedure; the uplink/downlink timing is fixed
        // (cl. 9.3.9) and the grant here is "next opportunity" (cl. 23.5.2.2.2),
        // so the target slot is exactly dltime + 2, computed above. We mark the
        // request `reserved_access: true`; PhyMs transmits it at exactly
        // `ul_time` (with discontinuous TX the exact slot is reachable) rather
        // than frame-advancing it into an unreserved slot.
        let blk = TmvUnitdataReq {
            mac_block: block,
            logical_channel,
            scrambling_code: frag.scrambling_code,
        };
        let m = SapMsg {
            sap: Sap::TmvSap,
            src: self.self_component,
            dest: TetraEntity::Lmac,
            msg: SapMsgInner::TmvUnitdataReq(TmvUnitdataReqSlot {
                ts: ul_time,
                ul_phy_chan: PhysicalChannel::Cp,
                blk1: Some(blk),
                blk2: None,
                bbk: None,
                // Reserved-access burst: the BS granted this exact slot
                // (cl. 23.5.2.2.2). The PHY must transmit it at exactly `ul_time`
                // or drop it -- it may not be frame-advanced to a later slot.
                reserved_access: true,
            }),
        };
        queue.push_back(m);

        // The whole fragmented TM-SDU is now handed to the PHY for transmission
        // in the reserved slot. Mark the LLC transmit receipt so the
        // acknowledged basic link starts its ack-wait/retransmit timer
        // (cl. 22.3.2.3); panic-safe against a re-presented retransmission.
        if let Some(tx_reporter) = frag.tx_reporter.as_ref() {
            tx_reporter.try_mark_transmitted();
        }
    }

    /// Drive an in-flight multi-slot reserved uplink transfer (ETSI TS 100 392-2
    /// cl. 23.4.2.1.2 / 23.5.2). Called each downlink slot (from `tick_start`,
    /// and once from `emit_fragment_end` when the plan is created): when the
    /// front reserved slot's uplink time (`dltime + 2`) is reached, the next
    /// prebuilt full-slot block (a MAC-FRAG-UL continuation, or the final
    /// MAC-END-UL) is emitted to LMAC as a reserved-access burst that the PHY
    /// transmits at exactly that slot.
    ///
    /// The reserved slots are spaced one TDMA frame apart (>= 4 timeslots) and
    /// the downlink clock advances one slot per tick, so at most one block is
    /// ever due in a given tick. If a reserved slot is missed (we advanced past
    /// it without transmitting — e.g. the downlink clock was re-seeded by a SYNC
    /// burst), the whole plan is abandoned and its transmit receipt discarded so
    /// the LLC/MM retransmit the whole transfer.
    fn drive_reserved_tx(&mut self, queue: &mut MessageQueue) {
        let Some(mut plan) = self.reserved_tx.take() else {
            return;
        };
        // The uplink slot paired with the current downlink slot (cl. 9.3.9).
        let ul = self.dltime.add_timeslots(2);
        let Some(&slot) = plan.slots.front() else {
            // An empty plan should never persist; drop it defensively.
            return;
        };

        // slot - ul, via ul.diff(slot) = ul - slot: >0 => ul past slot (missed);
        // 0 => this slot; <0 => still in the future.
        let delta = ul.diff(slot);
        if delta < 0 {
            // Reserved slot not reached yet; keep waiting.
            self.reserved_tx = Some(plan);
            return;
        }
        if delta > 0 {
            tracing::warn!(
                "uplink fragmentation: missed reserved slot {:?} (now UL {:?}); abandoning \
                 multi-slot transfer ({} block(s) unsent), LLC/MM will retransmit",
                slot,
                ul,
                plan.blocks.len()
            );
            if let Some(tx_reporter) = plan.tx_reporter.as_ref() {
                tx_reporter.try_mark_discarded();
            }
            // Plan dropped (taken, not restored).
            return;
        }

        // delta == 0: transmit the next block in its reserved slot.
        let block = plan
            .blocks
            .pop_front()
            .expect("blocks and slots kept in lockstep");
        plan.slots.pop_front();
        let is_last = plan.blocks.is_empty();
        tracing::info!(
            "uplink fragmentation: transmitting reserved {} at UL {:?} on SchF ({} block(s) left)",
            if is_last { "MAC-END-UL" } else { "MAC-FRAG-UL" },
            slot,
            plan.blocks.len()
        );

        let blk = TmvUnitdataReq {
            mac_block: block,
            logical_channel: LogicalChannel::SchF,
            scrambling_code: plan.scrambling_code,
        };
        let m = SapMsg {
            sap: Sap::TmvSap,
            src: self.self_component,
            dest: TetraEntity::Lmac,
            msg: SapMsgInner::TmvUnitdataReq(TmvUnitdataReqSlot {
                ts: slot,
                ul_phy_chan: PhysicalChannel::Cp,
                blk1: Some(blk),
                blk2: None,
                bbk: None,
                // Reserved-access burst: the BS reserved this exact slot
                // (cl. 23.5.2). The PHY must transmit it at exactly `slot` or
                // drop it -- it may not be frame-advanced to a later slot.
                reserved_access: true,
            }),
        };
        queue.push_back(m);

        if is_last {
            // The whole fragmented TM-SDU is now handed to the PHY. Mark the LLC
            // transmit receipt so the acknowledged basic link starts its
            // ack-wait / retransmit timer (cl. 22.3.2.3); panic-safe against a
            // re-presented retransmission. Plan complete (not restored).
            if let Some(tx_reporter) = plan.tx_reporter.as_ref() {
                tx_reporter.try_mark_transmitted();
            }
        } else {
            self.reserved_tx = Some(plan);
        }
    }

    /// Emit the MS uplink burst on the assigned traffic slot for this timeslot:
    /// continuous TCH/S speech while this MS is the talker, and/or a stolen
    /// half-slot (FACCH/STCH, cl. 23) carrying associated signalling.
    ///
    /// Mirrors the BS downlink traffic emitter (`lmac_bs` NDB path) for the
    /// uplink direction. UMAC is the transmit timing authority (ETSI TS 100
    /// 392-2 cl. 23): once per timeslot it decides whether a burst is due and
    /// clocks exactly one down to the LMAC, which channel-codes it into a Normal
    /// Uplink Burst (cl. 9.4.4.2). The speech stream is supplied by CC-MS (the
    /// U-plane owner, cl. 14.5.1.4) via the jitter buffer; on underrun a silence
    /// frame is emitted so a granted slot is never left dark mid-call.
    ///
    /// A burst is emitted on the paired uplink slot (`dltime + 2`, cl. 9.3.9)
    /// only when all of the following hold:
    /// - the slot is one the network assigned to us as a traffic channel
    ///   (`assigned_traffic_slots`, cl. 21.5.2) — never hardcoded to a fixed
    ///   timeslot — and it is not the frame-18 control frame (cl. 9.5.1c);
    /// - a valid serving-cell scrambling code is known (we are camped); and
    /// - either we hold the transmission grant (`uplink_tx_granted`, the talker,
    ///   cl. 14.5.1.4) — emitting speech — or associated signalling is queued to
    ///   steal a half-slot (e.g. a U-TX-DEMAND floor request while listening).
    ///   With neither, a non-talking party stays silent.
    ///
    /// When both hold (talking and a signalling PDU is queued) the slot is
    /// stolen: the STCH signalling block takes the first half and the TCH/S
    /// speech the second (normal training sequence 2, cl. 9.4.4.3.2).
    fn drive_uplink_traffic(&mut self, queue: &mut MessageQueue) {
        // The uplink slot paired with the current downlink slot (cl. 9.3.9).
        let ul = self.dltime.add_timeslots(2);

        // Frame 18 is the control frame — no traffic (cl. 9.5.1c / 23.4.2.1).
        if ul.f == 18 {
            return;
        }

        // Only emit on a timeslot the network assigned to this MS as a traffic
        // channel (cl. 21.5.2). The uplink slot number equals the downlink slot
        // number it is paired with, so the same assignment record applies.
        let ts_idx = ul.t as usize - 1;
        if !(1..=4).contains(&ul.t) || !self.assigned_traffic_slots[ts_idx] {
            return;
        }

        // Two reasons to key the transmitter on the assigned uplink slot:
        //  - we hold the transmission grant (talker) → emit continuous TCH/S; or
        //  - associated signalling is queued to steal a half-slot (FACCH/STCH,
        //    cl. 23) — e.g. a U-TX-DEMAND floor request while still listening.
        // With neither, stay silent (a non-talking party does not transmit).
        let has_steal = !self.pending_stolen_signalling.is_empty();
        if !self.uplink_tx_granted && !has_steal {
            return;
        }

        // Uplink bursts are scrambled with the serving cell's code, known only
        // once camped (set from the received SYNC). Without it we cannot form a
        // decodable burst, so stay silent.
        let Some(scrambling_code) = self.scrambling_code else {
            return;
        };

        // The TCH/S speech half. When we are the talker, take the next queued
        // U-plane source frame or synthesise silence on underrun (all-zero
        // 274-bit type-1 block, mirroring the BS downlink silence path). When we
        // are only stealing a slot to signal (not the talker) there is no speech
        // to send, so the speech half is silence. Non-silence frames arrive as
        // packed bytes; `BitBuffer::from_vec` wraps them and we clamp to the
        // TCH/S type-1 size.
        let mac_block = match self.uplink_audio.pop_front() {
            Some(frame) if self.uplink_tx_granted && !frame.is_empty() => {
                let mut buf = BitBuffer::from_vec(frame);
                let end = (buf.get_raw_start() + TCH_S_TYPE1_BITS).min(buf.get_raw_end());
                buf.set_raw_end(end);
                buf
            }
            _ => BitBuffer::new(TCH_S_TYPE1_BITS),
        };

        let blk = TmvUnitdataReq {
            mac_block,
            logical_channel: LogicalChannel::TchS,
            scrambling_code,
        };

        // If associated signalling is queued for FACCH/STCH stealing (cl. 23),
        // steal the first half of this traffic slot: blk1 carries the STCH
        // signalling block, blk2 the remaining TCH/S speech half. The burst then
        // uses normal training sequence 2 (selected in the LMAC from the STCH
        // logical channel). Otherwise the whole slot carries TCH/S speech (blk1
        // only). Mark the stolen block's LLC receipt transmitted so the
        // acknowledged-mode basic link progresses (cl. 22.3.2.3).
        let (blk1, blk2) = match self.pending_stolen_signalling.pop_front() {
            Some(steal) => {
                if let Some(tx_reporter) = steal.tx_reporter {
                    tx_reporter.try_mark_transmitted();
                }
                let stch = TmvUnitdataReq {
                    mac_block: steal.mac_block,
                    logical_channel: LogicalChannel::Stch,
                    scrambling_code: steal.scrambling_code,
                };
                (stch, Some(blk))
            }
            None => (blk, None),
        };

        let m = SapMsg {
            sap: Sap::TmvSap,
            src: self.self_component,
            dest: TetraEntity::Lmac,
            msg: SapMsgInner::TmvUnitdataReq(TmvUnitdataReqSlot {
                ts: ul,
                // Traffic physical channel carries the TCH/S burst (cl. 9.4.4.2),
                // matching the BS which uses Tp for traffic and Cp otherwise.
                ul_phy_chan: PhysicalChannel::Tp,
                blk1: Some(blk1),
                blk2,
                bbk: None,
                // Continuous traffic is clock-driven on the assigned slot, not a
                // BS-reserved random-access opportunity (cl. 23.5.2); the PHY may
                // schedule it at the paired uplink slot like BS downlink traffic.
                reserved_access: false,
            }),
        };
        queue.push_back(m);
    }

    pub fn rx_tmv_bsch(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_tmv_bsch");
        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };

        // Parse the MAC-SYNC PDU carried by the BSCH (ETSI TS 100 392-2 cl. 21.4.4.2).
        let pdu = match MacSync::from_bitbuf(&mut prim.pdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing MacSync: {:?} {}", e, prim.pdu.dump_bin());
                return;
            }
        };

        // Adopt the colour code. Together with the MCC/MNC provided by MLE over
        // TLMC, it derives the scrambling code (cl. 23.2.2 / 8.2.5).
        if self.cc != Some(pdu.colour_code) {
            tracing::info!("rx_tmv_bsch: colour code {:?} -> {}", self.cc, pdu.colour_code);
            self.cc = Some(pdu.colour_code);
        }

        // Seed the absolute downlink time from the SYNC burst (cl. 7 / 21.4.4.2).
        // UMAC free-runs this between SYNC bursts (see `tick_start`) and re-seeds
        // it here each frame 18.
        self.dltime = pdu.time;

        // Push the recovered time down to LMAC so it classifies logical channels
        // (BNCH / frame 18) and interprets AACH against the correct absolute time.
        let m = SapMsg {
            sap: Sap::TmvSap,
            src: self.self_component,
            dest: TetraEntity::Lmac,
            msg: SapMsgInner::TmvConfigureReq(TmvConfigureReq {
                time: Some(pdu.time),
                ..Default::default()
            }),
        };
        queue.push_back(m);

        // Forward the remaining bits (the D-MLE-SYNC SDU, cl. 18.4.2.1) up to MLE
        // for initial cell selection (cl. 18.3.4.6). MLE replies with a
        // TL-CONFIGURE carrying the valid MCC/MNC, which lets us derive the
        // scrambling code and submit it to LMAC.
        let tlsdu = BitBuffer::from_bitbuffer_pos(&prim.pdu);
        let m = SapMsg {
            sap: Sap::TlmbSap,
            src: TetraEntity::Umac,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::TlmbSyncInd(TlmbSyncInd {
                endpoint_id: 0,
                tl_sdu: tlsdu,
            }),
        };
        queue.push_back(m);
    }

    fn rx_tma_prim(&mut self, _queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tma_prim");
        let SapMsgInner::TmaUnitdataReq(mut prim) = message.msg else {
            panic!("rx_tma_prim: unexpected primitive");
        };

        // The MS can only transmit once it has camped on a cell and derived the
        // serving-cell scrambling code from SYNC (Phase 2). Without it we cannot
        // channel-encode an uplink burst.
        let Some(scrambling_code) = self.scrambling_code else {
            tracing::warn!("rx_tma_prim: no scrambling code yet (not camped), dropping uplink SDU");
            return;
        };

        let issi = self.own_issi();

        // The transmit receipt shared with the LLC acknowledged-mode outbound
        // entry (cl. 22.3.2.3). Marking it drives the LLC retransmit/give-up
        // logic; see PendingUplink::tx_reporter.
        let tx_reporter = prim.tx_reporter.take();

        // ── FACCH/STCH stealing on the granted traffic channel (cl. 23) ──────
        // When L3 requests channel stealing (`stealing_permission`) and this MS
        // has a traffic channel assigned for the call, the associated signalling
        // (e.g. U-TX-CEASED while talking, or U-TX-DEMAND while listening) must be
        // carried on the TCH by stealing a half-slot (STCH) — as acknowledged
        // BL-DATA on the TCH-associated basic link — rather than contending for
        // MCCH random access. Wrap the LLC SDU in a MAC-DATA PDU (cl. 21.4.3.3)
        // and queue it for `drive_uplink_traffic` to emit in the stolen first
        // half of the next burst on the assigned uplink slot. If no traffic
        // channel is assigned yet (pre-TCH floor request) we fall through to the
        // MCCH access path below, which now carries the PDU as acknowledged
        // BL-DATA too.
        if prim.stealing_permission && self.has_assigned_traffic_slot() {
            prim.pdu.seek(0);
            match Self::build_stch_mac_data_block(issi, &mut prim.pdu) {
                Some(mac_block) => {
                    tracing::info!(
                        "rx_tma_prim: stealing granted traffic slot for associated signalling \
                         (STCH MAC-DATA, {} SDU bits) for ISSI {}",
                        prim.pdu.get_len(),
                        issi
                    );
                    self.pending_stolen_signalling.push_back(PendingStolenSignalling {
                        mac_block,
                        scrambling_code,
                        tx_reporter,
                    });
                    return;
                }
                None => {
                    tracing::warn!(
                        "rx_tma_prim: signalling SDU too large to steal a single STCH half-slot; \
                         falling back to MCCH random access"
                    );
                    // Fall through to the MCCH access path with the receipt intact.
                }
            }
        }

        // L3-specified PDU priority + emergency flag for the random-access gate
        // (cl. 23.5.1.4.4). Read before `prim.pdu` is consumed below.
        let pdu_priority = prim.pdu_priority;
        let is_emergency = prim.is_emergency;
        // Build a MAC-ACCESS carrying the TM-SDU for random access on SCH/HU
        // (Control Uplink Burst). ETSI TS 100 392-2 cl. 21.4.2.1, cl. 23.5.1.
        // If the TM-SDU is too large for a single access burst (> ~62 bits on a
        // pi/4-DQPSK channel), fragment it into a MAC-ACCESS start-of-
        // fragmentation (carrying a capacity request) plus a MAC-END-HU
        // remainder (cl. 23.4.2.1.2, form i): the first fragment is sent by
        // random access and the remainder by reserved access once the BS grants
        // a subslot (cl. 23.5.2).
        let sdu_len = prim.pdu.get_len();
        prim.pdu.seek(0);
        let (new_pending, new_fragment) = match Self::build_mac_access_block(issi, &mut prim.pdu) {
            Some(mac_block) => {
                // Fits a single access burst; no fragmentation.
                tracing::debug!(
                    "rx_tma_prim: built MAC-ACCESS uplink for ISSI {} ({} SDU bits)",
                    issi,
                    sdu_len
                );
                (
                    PendingUplink {
                        mac_block,
                        logical_channel: LogicalChannel::SchHu,
                        scrambling_code,
                        tx_reporter,
                        pdu_priority,
                        is_emergency,
                    },
                    None,
                )
            }
            None => {
                // Oversized: fragment into MAC-ACCESS (frag start) + MAC-END-HU
                // (subslot) or MAC-END-UL (full slot), chosen by the remainder
                // size in `build_mac_access_frag_start` (cl. 23.4.2.1.2).
                prim.pdu.seek(0);
                match Self::build_mac_access_frag_start(issi, &mut prim.pdu) {
                    Some((frag_block, remainder, end_kind)) => {
                        let first_bits = sdu_len - remainder.get_len();
                        tracing::info!(
                            "rx_tma_prim: TM-SDU {} bits exceeds a single MAC-ACCESS; fragmenting into \
                             MAC-ACCESS frag-start ({} SDU bits, capacity request) + {:?} ({} SDU bits) \
                             for ISSI {}",
                            sdu_len,
                            first_bits,
                            end_kind,
                            remainder.get_len(),
                            issi
                        );
                        // The receipt travels with the completing fragment end
                        // (the whole TM-SDU is only "transmitted" once the
                        // remainder is on air); the frag-start carries none.
                        (
                            PendingUplink {
                                mac_block: frag_block,
                                logical_channel: LogicalChannel::SchHu,
                                scrambling_code,
                                tx_reporter: None,
                                pdu_priority,
                                is_emergency,
                            },
                            Some(UplinkFragment {
                                remainder,
                                scrambling_code,
                                end_kind,
                                tx_reporter,
                            }),
                        )
                    }
                    None => {
                        tracing::warn!(
                            "rx_tma_prim: TM-SDU ({} bits) exceeds the maximum multi-slot uplink \
                             fragment capacity ({} full slots); dropping (LLC/MM will retransmit)",
                            sdu_len,
                            MAX_UL_FRAG_SLOTS
                        );
                        return;
                    }
                }
            }
        };

        // Do not overwrite an uplink transfer that is already contending for
        // random access (or whose fragment remainder is still outstanding): the
        // in-flight `pending_uplink`/`pending_fragment` holds a `tx_reporter`
        // shared with the LLC acknowledged-mode outbound entry, and clobbering
        // it would orphan that receipt so it is never marked transmitted. The
        // LLC would then never set `t_umac_done`, its T251/N252
        // retransmit-and-give-up would never run, and the basic link would wedge
        // (observed as endless "still blocked" when an LLC BL-ACK auto-ack
        // interleaved with a group-attach). Queue the new transfer instead and
        // let `promote_next_uplink` start it once the current one completes.
        if self.uplink_in_flight() {
            if self.uplink_queue.len() >= MAX_UPLINK_QUEUE {
                if let Some(dropped) = self.uplink_queue.pop_front() {
                    tracing::warn!(
                        "rx_tma_prim: uplink queue full ({}); dropping oldest queued transfer \
                         (LLC will retransmit)",
                        MAX_UPLINK_QUEUE
                    );
                    Self::discard_queued_uplink(&dropped);
                }
            }
            tracing::debug!(
                "rx_tma_prim: uplink in flight; queueing transfer for ISSI {} ({} now waiting)",
                issi,
                self.uplink_queue.len() + 1
            );
            self.uplink_queue.push_back(QueuedUplink {
                pending: new_pending,
                fragment: new_fragment,
            });
        } else {
            self.pending_fragment = new_fragment;
            self.pending_uplink = Some(new_pending);
        }

        // Queue for transmission at the next valid random-access opportunity.
        // The MS-MAC random access procedure (cl. 23.5.1.4) — access-frame
        // selection and the randomised access algorithm — is initiated and
        // driven by `drive_random_access` on each downlink slot carrying a
        // valid ACCESS-ASSIGN, once access parameters for code A have been
        // advertised (SYSINFO default-A / ACCESS-DEFINE, cl. 21.4.4.1). Holding
        // the block here rather than requiring the parameters to already be
        // present avoids dropping the first uplink when the trigger — e.g. MM
        // registration on cell selection — fires before the broadcast carrying
        // the access parameters has been received. The actual transmit is
        // emitted to LMAC and PHY from `drive_random_access`. If the SDU was
        // fragmented, the MAC-END-HU remainder is emitted from `rx_mac_resource`
        // when the BS grants a subslot in response to the frag-start.
    }

    /// Mark every transmit receipt carried by a queued (never-transmitted)
    /// uplink as discarded (cl. 22.3.2.3) so the LLC retransmits it. Used when a
    /// queued transfer is dropped on overflow without ever reaching the air.
    fn discard_queued_uplink(dropped: &QueuedUplink) {
        if let Some(tx_reporter) = dropped.pending.tx_reporter.as_ref() {
            tx_reporter.try_mark_discarded();
        }
        if let Some(fragment) = dropped.fragment.as_ref() {
            if let Some(tx_reporter) = fragment.tx_reporter.as_ref() {
                tx_reporter.try_mark_discarded();
            }
        }
    }

    /// Whether an uplink transfer is currently occupying the single random-access
    /// / reserved-transmission slot the MS-MAC allows (cl. 23.5.1.4 / 23.4.2.1.2).
    /// A new transfer must not start (or overwrite) while any of these hold, or
    /// its `tx_reporter` would be orphaned and the acknowledged basic link would
    /// wedge. Covers a queued/contending MAC-ACCESS (`pending_uplink`), an
    /// outstanding fragment remainder awaiting its grant (`pending_fragment`), an
    /// active random-access attempt, and an in-flight multi-slot reserved transfer
    /// (`reserved_tx`).
    fn uplink_in_flight(&self) -> bool {
        self.pending_uplink.is_some()
            || self.pending_fragment.is_some()
            || self.random_access.is_active()
            || self.reserved_tx.is_some()
    }

    /// Promote the next queued uplink transfer (if any) into the active
    /// `pending_uplink`/`pending_fragment` slots, but only when nothing is
    /// currently contending for random access. Called each downlink slot from
    /// `drive_random_access`, this is what lets a transfer queued behind another
    /// (e.g. a group-attach behind an LLC BL-ACK) start once the earlier one has
    /// been acknowledged or abandoned.
    fn promote_next_uplink(&mut self) {
        if self.uplink_in_flight() {
            return;
        }
        if let Some(next) = self.uplink_queue.pop_front() {
            tracing::debug!(
                "promote_next_uplink: starting queued uplink transfer ({} still waiting)",
                self.uplink_queue.len()
            );
            self.pending_fragment = next.fragment;
            self.pending_uplink = Some(next.pending);
        }
    }

    /// Build a MAC-ACCESS type-1 MAC block (ETSI TS 100 392-2 cl. 21.4.2.1)
    /// carrying `sdu` for transmission on SCH/HU (Control Uplink Burst). The
    /// block is addressed with the MS's own ISSI (the uplink MAC-ACCESS address
    /// is always an ISSI, cl. 21.4.2.1). Returns the full 92-bit type-1 block
    /// (MAC-ACCESS header + TM-SDU + fill bits), or `None` if the SDU does not
    /// fit a single access burst (uplink fragmentation, cl. 23.4.2.1, not yet
    /// implemented).
    ///
    /// The PDU carries **no length indication**: per cl. 21.4.2.1 the length
    /// indication field "should be used only if association within the uplink
    /// subslot is required or for transmission of the null PDU", neither of
    /// which applies to a self-contained random-access signalling burst. With
    /// the optional field flag left at 0, the MAC-ACCESS implicitly spans the
    /// whole MAC block and the remaining capacity is completed with fill bits
    /// (cl. 23.4.2.2: a bit "1" immediately after the TM-SDU followed by bits
    /// "0" to the end of the MAC block). The receiver (cl. 23.4.3.2) treats the
    /// PDU as filling the block and strips the trailing fill bits, so the
    /// padding is never mis-decoded as a spurious second concatenated MAC PDU.
    /// True when the network has assigned this MS at least one uplink traffic
    /// slot via a same-carrier CHANNEL ALLOCATION element (cl. 21.5.2). Gates
    /// FACCH/STCH stealing: without a granted traffic channel there is nothing to
    /// steal, so associated signalling must go over the MCCH instead.
    fn has_assigned_traffic_slot(&self) -> bool {
        self.assigned_traffic_slots.iter().any(|&s| s)
    }

    /// Build a 124-bit STCH type-1 MAC block wrapping `sdu` (an LLC BL-DATA PDU)
    /// in a MAC-DATA header (ETSI TS 100 392-2 cl. 21.4.3.3) for FACCH/STCH
    /// half-slot stealing on the granted uplink traffic channel (cl. 23).
    ///
    /// Mirrors the BS downlink FACCH builder (`rx_ul_tma_unitdata_req` STCH path)
    /// but for the uplink direction, where the MS uses MAC-DATA rather than the
    /// downlink-only MAC-RESOURCE. The length indication covers header + SDU +
    /// sub-octet fill (cl. 23.4.2.2); bits beyond `length_ind` octets are ignored
    /// by the receiver. Returns `None` if the PDU does not fit a single STCH
    /// half-slot, in which case the caller falls back to MCCH random access.
    fn build_stch_mac_data_block(issi: u32, sdu: &mut BitBuffer) -> Option<BitBuffer> {
        const STCH_TYPE1_BITS: usize = 124;

        let mut pdu = MacData {
            fill_bits: false,
            encrypted: false,
            addr: Some(TetraAddress {
                ssi_type: SsiType::Issi,
                ssi: issi,
            }),
            event_label: None,
            // Complete (non-fragmented) PDU: carry an explicit length indication
            // rather than a capacity request (cl. 21.4.3.3). Filled in below.
            length_ind: Some(0),
            frag_flag: None,
            reservation_req: None,
        };

        let sdu_len = sdu.get_len();
        let num_fill_bits = pdu.update_len_and_fill_ind(sdu_len);
        let content_len = pdu.length_ind.unwrap() as usize * 8;
        if content_len > STCH_TYPE1_BITS {
            // Does not fit a single stolen half-slot.
            return None;
        }

        let mut block = BitBuffer::new(STCH_TYPE1_BITS);
        pdu.to_bitbuf(&mut block);
        sdu.seek(0);
        block.copy_bits(sdu, sdu_len);
        fillbits::addition::write(&mut block, Some(num_fill_bits));
        block.seek(0);
        Some(block)
    }

    pub fn build_mac_access_block(issi: u32, sdu: &mut BitBuffer) -> Option<BitBuffer> {
        let mut pdu = MacAccess {
            fill_bits: false,
            encrypted: false,
            addr: Some(TetraAddress {
                ssi_type: SsiType::Issi,
                ssi: issi,
            }),
            event_label: None,
            // No length indication and no capacity request: optional field
            // flag = 0 (cl. 21.4.2.1). The PDU implicitly fills the MAC block.
            length_ind: None,
            frag_flag: None,
            reservation_req: None,
        };

        // Measure the header length. The fill_bits flag does not change the
        // number of header bits (30 bits with an ISSI and no optional field).
        let hdr_len = {
            let mut scratch = BitBuffer::new(64);
            pdu.to_bitbuf(&mut scratch);
            scratch.get_pos()
        };

        let sdu_len = sdu.get_len();
        let content_len = hdr_len + sdu_len;
        if content_len > SCH_HU_TYPE1_BITS {
            // Does not fit a single access burst; would require uplink
            // fragmentation (MAC-ACCESS frag start + MAC-END-HU, cl. 23.4.2.1),
            // which is not yet implemented.
            return None;
        }

        // Fill bits complete the block whenever the content is shorter than the
        // available MAC-block capacity (cl. 23.4.2.2). The "fill bit indication"
        // is set to 1 iff any fill bits are present.
        let num_fill_bits = SCH_HU_TYPE1_BITS - content_len;
        pdu.fill_bits = num_fill_bits != 0;

        // Assemble the full 92-bit type-1 block: MAC-ACCESS header + TM-SDU +
        // fill bits ("1" then "0"s to the end of the MAC block, cl. 23.4.2.2).
        let mut block = BitBuffer::new(SCH_HU_TYPE1_BITS);
        pdu.to_bitbuf(&mut block);
        sdu.seek(0);
        block.copy_bits(sdu, sdu_len);
        fillbits::addition::write(&mut block, None);
        block.seek(0);
        Some(block)
    }

    /// Largest TM-SDU remainder (in bits) that a single MAC-END-HU can carry on
    /// an SCH/HU subslot: header + SDU + fill must be a whole number of octets
    /// not exceeding the 92-bit block (cl. 21.4.2.2 / 23.4.2.2).
    const fn max_end_hu_sdu_bits() -> usize {
        (SCH_HU_TYPE1_BITS / 8) * 8 - MAC_END_HU_HEADER_BITS
    }

    /// Largest TM-SDU remainder (in bits) that a single MAC-END-UL can carry on
    /// an SCH/F full slot: header + SDU + fill must be a whole number of octets
    /// not exceeding the 268-bit block (cl. 21.4.2.5 / 23.4.2.2).
    const fn max_end_ul_sdu_bits() -> usize {
        (SCH_F_TYPE1_BITS / 8) * 8 - MAC_END_UL_HEADER_BITS
    }

    /// TM-SDU bits carried by a single MAC-FRAG-UL continuation: the whole
    /// 268-bit SCH/F block minus the 4-bit header (cl. 21.4.2.4). A continuation
    /// fills its slot exactly (no fill bits, no length indication — the length
    /// is carried by the terminating MAC-END-UL).
    const fn max_frag_ul_sdu_bits() -> usize {
        SCH_F_TYPE1_BITS - MAC_FRAG_UL_HEADER_BITS
    }

    /// Number of full SCH/F slots a multi-slot uplink fragment remainder needs:
    /// `total_slots - 1` MAC-FRAG-UL continuations of [`Self::max_frag_ul_sdu_bits`]
    /// bits each plus a terminating MAC-END-UL of at most
    /// [`Self::max_end_ul_sdu_bits`] bits (ETSI TS 100 392-2 cl. 23.4.2.1.2).
    /// Returns `None` if the remainder needs more than [`MAX_UL_FRAG_SLOTS`]
    /// slots. The caller only reaches this for a remainder that does not fit a
    /// single MAC-END-UL, so `total_slots >= 2`.
    fn compute_ul_frag_slots(remainder_len: usize) -> Option<usize> {
        let frag = Self::max_frag_ul_sdu_bits();
        let end = Self::max_end_ul_sdu_bits();
        debug_assert!(remainder_len > end, "single MAC-END-UL should have been chosen");
        // ceil((remainder - end) / frag) continuations, each of `frag` bits,
        // leave a tail of at most `end` bits for the MAC-END-UL.
        let k = (remainder_len - end).div_ceil(frag);
        let total_slots = k + 1;
        if total_slots > MAX_UL_FRAG_SLOTS {
            None
        } else {
            Some(total_slots)
        }
    }

    /// Build one MAC-FRAG-UL continuation block (ETSI TS 100 392-2 cl. 21.4.2.4)
    /// on a reserved SCH/F full slot: the 4-bit header followed by exactly
    /// [`Self::max_frag_ul_sdu_bits`] TM-SDU bits, filling the 268-bit block with
    /// no fill bits and no length indication (the terminating MAC-END-UL carries
    /// the overall length). `sdu_chunk` must hold exactly that many bits.
    pub fn build_mac_frag_ul_block(sdu_chunk: &mut BitBuffer) -> BitBuffer {
        debug_assert_eq!(sdu_chunk.get_len(), Self::max_frag_ul_sdu_bits());
        let pdu = MacFragUl { fill_bits: false };
        let mut block = BitBuffer::new(SCH_F_TYPE1_BITS);
        pdu.to_bitbuf(&mut block);
        sdu_chunk.seek(0);
        block.copy_bits(sdu_chunk, sdu_chunk.get_len());
        block.seek(0);
        block
    }

    /// Split a multi-slot fragment `remainder` into `total_slots` full-slot
    /// blocks: `total_slots - 1` MAC-FRAG-UL continuations followed by a
    /// terminating MAC-END-UL (ETSI TS 100 392-2 cl. 23.4.2.1.2). Returns the
    /// blocks in transmission order, or `None` if the sizing is inconsistent
    /// (should not happen for a `total_slots` from [`Self::compute_ul_frag_slots`]).
    pub fn build_ul_frag_blocks(remainder: &mut BitBuffer, total_slots: usize) -> Option<Vec<BitBuffer>> {
        let frag = Self::max_frag_ul_sdu_bits();
        let k = total_slots.checked_sub(1)?; // MAC-FRAG-UL continuations
        let r = remainder.get_len();
        if r < k * frag {
            return None;
        }
        let tail = r - k * frag;
        if tail == 0 || tail > Self::max_end_ul_sdu_bits() {
            return None;
        }
        remainder.seek(0);
        let mut blocks = Vec::with_capacity(total_slots);
        for _ in 0..k {
            let mut chunk = BitBuffer::new(frag);
            chunk.copy_bits(remainder, frag);
            blocks.push(Self::build_mac_frag_ul_block(&mut chunk));
        }
        let mut tail_buf = BitBuffer::new(tail);
        tail_buf.copy_bits(remainder, tail);
        blocks.push(Self::build_mac_end_ul_block(&mut tail_buf)?);
        Some(blocks)
    }

    /// The sequence of reserved uplink slots for a multi-slot transfer, starting
    /// at `first` (the `dltime + 2` uplink slot paired with the downlink slot
    /// carrying the grant, cl. 9.3.9 / 23.5.2.2.2). Subsequent slots are the
    /// same-numbered timeslot in each following TDMA frame (step +4 timeslots),
    /// skipping any mandatory CLCH slot — mirroring the BS
    /// `ul_find_grant_opportunity` "capacity allocation at next opportunity"
    /// stepping so the MS reproduces the BS's reserved set exactly.
    fn reserved_slot_sequence(first: TdmaTime, total_slots: usize) -> VecDeque<TdmaTime> {
        let mut slots = VecDeque::with_capacity(total_slots);
        let mut cand = first;
        while slots.len() < total_slots {
            if !cand.is_mandatory_clch() {
                slots.push_back(cand);
            }
            cand = cand.add_timeslots(4);
        }
        slots
    }

    /// Build the first fragment of an oversized uplink TM-SDU: a MAC-ACCESS
    /// "start of fragmentation" block (ETSI TS 100 392-2 cl. 21.4.2.1,
    /// cl. 23.4.2.1.2). The block is addressed with the MS's own ISSI and
    /// carries a **capacity request** — the fragmentation flag set plus a
    /// "reservation requirement" sized to the remainder — so the base station
    /// grants uplink capacity for the remainder (cl. 23.5.2): one subslot
    /// (`Req1Subslot`) for a MAC-END-HU remainder, or one full slot
    /// (`Req1Slot`) for a MAC-END-UL remainder. The first fragment carries as
    /// many TM-SDU bits as exactly fill the MAC block after the header, so there
    /// are no fill bits (cl. 23.4.2.1.2).
    ///
    /// Returns the full 92-bit type-1 block, a `BitBuffer` holding the remaining
    /// TM-SDU bits, and the [`FragEndKind`] the remainder must be completed
    /// with. Returns `None` if the SDU is not actually larger than one fragment
    /// (caller should use the self-contained [`Self::build_mac_access_block`]
    /// path instead), or if the remainder is larger than
    /// [`MAX_UL_FRAG_SLOTS`] full slots can carry (no realistic signalling SDU
    /// is that large).
    pub fn build_mac_access_frag_start(
        issi: u32,
        sdu: &mut BitBuffer,
    ) -> Option<(BitBuffer, BitBuffer, FragEndKind)> {
        let sdu_len = sdu.get_len();

        let mut pdu = MacAccess {
            fill_bits: false,
            encrypted: false,
            addr: Some(TetraAddress {
                ssi_type: SsiType::Issi,
                ssi: issi,
            }),
            event_label: None,
            length_ind: None,
            // Capacity request (cl. 21.4.2.1): fragmentation flag set marks this
            // as the start of a fragmented transfer; the reservation requirement
            // (chosen below from the remainder size) asks the BS for the uplink
            // capacity to send the fragment end (cl. 23.5.2 / Table 21.55). The
            // reservation-requirement field is a fixed 4 bits, so the header
            // length does not depend on the value used here.
            frag_flag: Some(true),
            reservation_req: Some(ReservationRequirement::Req1Subslot),
        };

        // Header length (36 bits: ISSI + optional field flag + capacity request).
        let hdr_len = {
            let mut scratch = BitBuffer::new(64);
            pdu.to_bitbuf(&mut scratch);
            scratch.get_pos()
        };

        // The first fragment carries exactly enough TM-SDU to fill the MAC block
        // after the header (no fill bits, cl. 23.4.2.1.2).
        let frag_bits = SCH_HU_TYPE1_BITS - hdr_len;
        if frag_bits == 0 || sdu_len <= frag_bits {
            // Not actually oversized for a fragmented transfer.
            return None;
        }
        let remainder_len = sdu_len - frag_bits;

        // Choose the fragment-end PDU and matching capacity request from the
        // remainder size (cl. 23.4.2.1.2 / 23.5.2): a small remainder fits a
        // MAC-END-HU on one granted subslot; a larger one needs a MAC-END-UL on
        // a granted SCH/F full slot; a still larger one is split across several
        // reserved full slots as MAC-FRAG-UL continuations terminated by a
        // MAC-END-UL (true multi-slot fragmentation), requesting the matching
        // N-slot reserved capacity (cl. 23.5.2 basic slot granting).
        let (reservation_req, end_kind) = if remainder_len <= Self::max_end_hu_sdu_bits() {
            (ReservationRequirement::Req1Subslot, FragEndKind::MacEndHu)
        } else if remainder_len <= Self::max_end_ul_sdu_bits() {
            (ReservationRequirement::Req1Slot, FragEndKind::MacEndUl)
        } else {
            // Needs (N-1) MAC-FRAG-UL continuations (264 SDU bits each) plus a
            // terminating MAC-END-UL. `compute_ul_frag_slots` sizes N and caps
            // it at `MAX_UL_FRAG_SLOTS`; beyond that the transfer is rejected
            // (returns None) — no realistic MM/CMCE signalling SDU is that large.
            let n = Self::compute_ul_frag_slots(remainder_len)?;
            (
                ReservationRequirement::from_req_slotcount(n),
                FragEndKind::MacFragUl { total_slots: n },
            )
        };
        pdu.reservation_req = Some(reservation_req);

        // Assemble the 92-bit type-1 block: frag-start MAC-ACCESS header + first
        // `frag_bits` of the TM-SDU (fills the block exactly).
        let mut block = BitBuffer::new(SCH_HU_TYPE1_BITS);
        pdu.to_bitbuf(&mut block);
        sdu.seek(0);
        block.copy_bits(sdu, frag_bits);
        block.seek(0);

        // The remainder (TM-SDU bits after the first fragment); `sdu` is now
        // positioned at `frag_bits` from the copy above.
        let mut remainder = BitBuffer::new(remainder_len);
        remainder.copy_bits(sdu, remainder_len);
        remainder.seek(0);

        Some((block, remainder, end_kind))
    }

    /// Build the MAC-END-HU block that completes a fragmented uplink transfer
    /// (ETSI TS 100 392-2 cl. 21.4.2.2, cl. 23.4.2.1.2). It carries the
    /// `remainder` of the TM-SDU in a single granted SCH/HU subslot, terminated
    /// by a **length indication** (octet count) so the receiver reassembles
    /// exactly the original TM-SDU. Fill bits pad the PDU to a byte boundary
    /// (cl. 23.4.2.2): a bit "1" immediately after the TM-SDU followed by bits
    /// "0" to the next octet boundary. The remaining bits of the 92-bit block
    /// (beyond the length indication) are left zero and are ignored by the
    /// receiver (it reads only `length_ind * 8` bits).
    ///
    /// Returns the full 92-bit type-1 block, or `None` if the remainder is too
    /// large for one MAC-END-HU (the caller then uses the full-slot MAC-END-UL
    /// path instead).
    pub fn build_mac_end_hu_block(remainder: &mut BitBuffer) -> Option<BitBuffer> {
        let sdu_len = remainder.get_len();
        let content_len = MAC_END_HU_HEADER_BITS + sdu_len;

        // Fill bits to the next byte boundary (the length indication counts
        // whole octets, cl. 21.4.2.2 / 23.4.2.2).
        let num_fill = fillbits::addition::compute_required_bytealigned(content_len);
        let total_len = content_len + num_fill;
        if total_len > SCH_HU_TYPE1_BITS {
            // Would not fit one MAC-END-HU block; needs a full-slot MAC-END-UL.
            return None;
        }
        debug_assert!(total_len % 8 == 0);
        let length_ind = (total_len / 8) as u8;

        let pdu = MacEndHu {
            fill_bits: num_fill != 0,
            length_ind: Some(length_ind),
            reservation_req: None,
        };

        let mut block = BitBuffer::new(SCH_HU_TYPE1_BITS);
        pdu.to_bitbuf(&mut block);
        remainder.seek(0);
        block.copy_bits(remainder, sdu_len);
        // Fill bits: "1" then "0"s to the octet boundary (cl. 23.4.2.2).
        if num_fill != 0 {
            fillbits::addition::write(&mut block, Some(num_fill));
        }
        // Pad the rest of the physical 92-bit block with zeros so the whole
        // type-1 block is defined for channel encoding; the receiver ignores
        // these bits (they are beyond `length_ind * 8`).
        let remaining = block.get_len_remaining();
        if remaining > 0 {
            block.write_zeroes(remaining);
        }
        block.seek(0);
        Some(block)
    }

    /// Build the MAC-END-UL block that completes a fragmented uplink transfer on
    /// a granted SCH/F full slot (ETSI TS 100 392-2 cl. 21.4.2.5,
    /// cl. 23.4.2.1.2). It carries the `remainder` of the TM-SDU in the 268-bit
    /// SCH/F block, terminated by a **length indication** (octet count) so the
    /// receiver reassembles exactly the original TM-SDU. Fill bits pad the PDU
    /// to a byte boundary (cl. 23.4.2.2). The remaining bits of the 268-bit
    /// block (beyond the length indication) are left zero and are ignored by the
    /// receiver (it reads only `length_ind * 8` bits).
    ///
    /// Returns the full 268-bit type-1 block, or `None` if the remainder is too
    /// large for one MAC-END-UL (which would require multi-slot fragmentation
    /// with MAC-FRAG-UL continuations — not implemented).
    pub fn build_mac_end_ul_block(remainder: &mut BitBuffer) -> Option<BitBuffer> {
        let sdu_len = remainder.get_len();
        let content_len = MAC_END_UL_HEADER_BITS + sdu_len;

        // Fill bits to the next byte boundary (the length indication counts
        // whole octets, cl. 21.4.2.5 / 23.4.2.2).
        let num_fill = fillbits::addition::compute_required_bytealigned(content_len);
        let total_len = content_len + num_fill;
        if total_len > SCH_F_TYPE1_BITS {
            // Would not fit one full slot; needs multi-slot fragmentation.
            return None;
        }
        debug_assert!(total_len % 8 == 0);
        let length_ind = (total_len / 8) as u8;

        let pdu = MacEndUl {
            fill_bits: num_fill != 0,
            length_ind: Some(length_ind),
            reservation_req: None,
        };

        let mut block = BitBuffer::new(SCH_F_TYPE1_BITS);
        // MAC-END-UL to_bitbuf validates its own fields; a length indication we
        // computed here is always in range, so this cannot fail in practice.
        pdu.to_bitbuf(&mut block).ok()?;
        remainder.seek(0);
        block.copy_bits(remainder, sdu_len);
        // Fill bits: "1" then "0"s to the octet boundary (cl. 23.4.2.2).
        if num_fill != 0 {
            fillbits::addition::write(&mut block, Some(num_fill));
        }
        // Pad the rest of the physical 268-bit block with zeros so the whole
        // type-1 block is defined for channel encoding; the receiver ignores
        // these bits (they are beyond `length_ind * 8`).
        let remaining = block.get_len_remaining();
        if remaining > 0 {
            block.write_zeroes(remaining);
        }
        block.seek(0);
        Some(block)
    }

    /// Take the MAC block queued for uplink transmission, if any. Consumed by
    /// the uplink PHY driver once a valid access opportunity is reached
    /// (Phase 3d).
    pub fn take_pending_uplink(&mut self) -> Option<PendingUplink> {
        self.pending_uplink.take()
    }

    /// The MS's own Individual Short Subscriber Identity (ISSI) from config.
    fn own_issi(&self) -> u32 {
        self.config
            .config()
            .ms
            .as_ref()
            .expect("MS config section required in MS mode")
            .issi
    }

    /// MS receive filter (ETSI TS 100 392-2 clause 23 addressing): decide
    /// whether a downlink MAC PDU carrying `addr` is destined for this mobile
    /// station.
    ///
    /// At the MAC layer an individual (ISSI) and a group (GSSI) identity are
    /// both carried as a bare 24-bit SSI — `MacResourceAddrType` has no
    /// distinct group code — so ownership is decided purely by SSI value: the
    /// MS accepts its own ISSI, any group it has attached to, and the
    /// broadcast address, and ignores traffic addressed to other
    /// subscribers/groups.
    ///
    /// Filtering applies ONLY in MS mode. In monitor mode (and any non-MS
    /// mode) the receiver is passive and must observe all traffic, so nothing
    /// is filtered.
    fn accept_downlink_address(&self, addr: &TetraAddress) -> bool {
        let cfg = self.config.config();
        if cfg.stack_mode != StackMode::Ms {
            // Monitor / non-MS mode: observe everything.
            return true;
        }
        // Broadcast address is always processed.
        if addr.ssi == BROADCAST_SSI {
            return true;
        }
        if cfg.ms.is_none() {
            // No MS section (should not happen in MS mode): preserve prior
            // unfiltered behaviour rather than silently dropping traffic.
            return true;
        }
        // Own individual identity or any currently-attached group. The runtime
        // sets are seeded from config and kept live by the MLE-IDENTITIES chain
        // (cl. 17.3.2), so dynamic group attach/detach changes what is accepted.
        self.valid_individual_ssi == Some(addr.ssi) || self.valid_group_ssis.contains(&addr.ssi)
    }

    fn rx_tlmb_prim(&mut self, _queue: &mut MessageQueue, _message: SapMsg) {
        tracing::trace!("rx_tlmb_prim");
        unimplemented!();
    }

    fn update_scrambing_and_submit_to_lmac(&mut self, queue: &mut MessageQueue) {
        if let (Some(mcc), Some(mnc), Some(cc)) = (self.mcc, self.mnc, self.cc) {
            self.scrambling_code = Some((((cc as u32) | ((mnc as u32) << 6) | ((mcc as u32) << 20)) << 2) | 3);

            tracing::trace!(
                "compute_scrambling_and_submit_to_lmac cc {} mcc {} mnc {} scrambling_code: {}",
                cc,
                mcc,
                mnc,
                self.scrambling_code.unwrap()
            );

            let m = SapMsg {
                sap: Sap::TmvSap,
                src: self.self_component,
                dest: TetraEntity::Lmac,
                msg: SapMsgInner::TmvConfigureReq(TmvConfigureReq {
                    scrambling_code: self.scrambling_code,
                    ..Default::default()
                }),
            };
            queue.push_back(m);
        }
    }

    fn rx_tlmc_configure_req(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tlmc_configure_req");
        let SapMsgInner::TlmcConfigureReq(prim) = &message.msg else {
            panic!()
        };

        if let Some(valid_addresses) = &prim.valid_addresses {
            tracing::debug!("rx_tlmc_configure_req: valid_addresses: {:?}", valid_addresses);

            self.mcc = Some(valid_addresses.mcc);
            self.mnc = Some(valid_addresses.mnc);

            // Update the runtime downlink address filter when the MLE supplies
            // the MS's identities (cl. 17.3.2 / 23.4.1.2.1). `None` members mean
            // "leave unchanged" (e.g. a scrambling-only configure at cell
            // selection), so the filter is only touched by an MLE-IDENTITIES
            // request carrying the attached set.
            if let Some(individual_ssi) = valid_addresses.individual_ssi {
                self.valid_individual_ssi = Some(individual_ssi);
            }
            if let Some(group_ssis) = &valid_addresses.group_ssis {
                self.valid_group_ssis = group_ssis.iter().copied().collect();
                tracing::info!(
                    "MS downlink address filter updated: issi={:?} groups={:?}",
                    self.valid_individual_ssi,
                    self.valid_group_ssis
                );
            }

            // Attempt to update scrambling code (if cc is also known)
            self.update_scrambing_and_submit_to_lmac(queue);
        } else {
            tracing::warn!("rx_tlmc_configure_req: No valid addresses provided");
        }
    }

    fn rx_tlmc_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tlmc_prim");
        match message.msg {
            SapMsgInner::TlmcConfigureReq(_) => {
                self.rx_tlmc_configure_req(queue, message);
            }
            SapMsgInner::TlmcTuneReq(_) => {
                self.rx_tlmc_tune_req(queue, message);
            }
            SapMsgInner::TlmcUPlaneConfigureReq(prim) => {
                // MLE-CONFIGURE U-plane transmit state (cl. 17.3.3 / 14.5.1.4),
                // forwarded down from CC-MS via MLE. This MS may emit uplink
                // TCH/S traffic only while it holds the transmission grant AND
                // the U-plane is switched on (it is the current talker). The
                // granted slot is not carried here — it stays owned by the
                // CHANNEL ALLOCATION record (cl. 21.5.2). On de-grant, flush the
                // uplink jitter buffer so a later grant cannot emit stale audio.
                let granted = prim.tx_grant && prim.switch_u_plane;
                if granted != self.uplink_tx_granted {
                    tracing::info!(
                        "UMAC-MS: U-plane transmit grant {} (tx_grant={}, switch_u_plane={})",
                        if granted { "acquired" } else { "released" },
                        prim.tx_grant,
                        prim.switch_u_plane
                    );
                }
                self.uplink_tx_granted = granted;
                if !granted {
                    self.uplink_audio.clear();
                }
            }
            _ => {
                panic!();
            }
        }
    }

    /// MS runtime downlink retune (**[impl policy]**): forward the MLE's tune
    /// request down to the lower MAC (TLMC -> TMV). UMAC holds no radio state for
    /// this; it is pure pass-through so the PHY ultimately retunes the SDR.
    fn rx_tlmc_tune_req(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        let SapMsgInner::TlmcTuneReq(prim) = &message.msg else {
            panic!()
        };
        let carrier_hz = prim.carrier_hz;
        tracing::info!("UMAC: forwarding MS downlink retune to {} Hz (TLMC -> TMV)", carrier_hz);
        queue.push_back(SapMsg {
            sap: Sap::TmvSap,
            src: TetraEntity::Umac,
            dest: TetraEntity::Lmac,
            msg: SapMsgInner::TmvTuneReq(TmvTuneReq { carrier_hz }),
        });
    }

    /// Act on a CHANNEL ALLOCATION element (cl. 21.5.2) received in a
    /// MAC-RESOURCE addressed to this MS, recording which downlink timeslot(s)
    /// carry our assigned traffic channel so the U-plane TMD relay follows the
    /// assigned slot (cl. 14.5.1.3). A same-carrier TCH is on some slot other
    /// than the control channel, so the slot is taken from the element and never
    /// hardcoded.
    ///
    /// M2 handles same-carrier allocations only. A cross-carrier or cell-change
    /// allocation (different `carrier_num`, `cell_change_flag`, or extended
    /// carrier numbering) requires a downlink PHY retune, which is deferred to
    /// M3 — here it is logged and ignored (no retune, no slot change), so the MS
    /// keeps decoding its current channel rather than acting half-way on an
    /// allocation it cannot yet follow.
    fn act_on_channel_allocation(&mut self, ca: &ChanAllocElement) {
        // Cross-cell or extended (cross-band) carrier numbering: defer to M3.
        if ca.ext.is_some() || ca.cell_change_flag {
            tracing::info!(
                "rx_mac_resource: cross-cell/extended CHANNEL ALLOCATION (cell_change={}, ext={}) deferred to M3; not following",
                ca.cell_change_flag,
                ca.ext.is_some()
            );
            return;
        }
        // Same-carrier classification (cl. 21.5.2): the allocation carrier must
        // equal the serving cell's downlink main carrier. A different carrier is
        // a cross-carrier retune, deferred to M3.
        match self.serving_carrier_num {
            Some(serving) if ca.carrier_num == serving => {}
            Some(serving) => {
                tracing::info!(
                    "rx_mac_resource: cross-carrier CHANNEL ALLOCATION (carrier {} != serving {}) deferred to M3; not following",
                    ca.carrier_num,
                    serving
                );
                return;
            }
            None => {
                // No SYSINFO decoded yet: cannot confirm same-carrier, so do not
                // act (conservative — avoids following an unclassifiable alloc).
                tracing::debug!("rx_mac_resource: CHANNEL ALLOCATION received before serving carrier known; ignoring");
                return;
            }
        }

        // Only a downlink-bearing assignment (Dl or Both) carries speech for us
        // to receive; an uplink-only grant assigns our transmit slot (M4) and
        // changes no downlink decode.
        if ca.ul_dl_assigned != UlDlAssignment::Dl && ca.ul_dl_assigned != UlDlAssignment::Both {
            tracing::debug!(
                "rx_mac_resource: same-carrier CHANNEL ALLOCATION is {} (no downlink); no traffic-slot change",
                ca.ul_dl_assigned
            );
            return;
        }

        // Apply the assignment per its type (cl. 21.5.2 / 14.8.17a):
        //  - Replace / QuitAndGo / ReplaceWithCarrierSignalling: this becomes the
        //    traffic channel — overwrite the assigned-slot set.
        //  - Additional: add the assigned slot(s) to the existing set.
        match ca.alloc_type {
            ChanAllocType::Additional => {
                for i in 0..4 {
                    self.assigned_traffic_slots[i] |= ca.ts_assigned[i];
                }
            }
            ChanAllocType::Replace
            | ChanAllocType::QuitAndGo
            | ChanAllocType::ReplaceWithCarrierSignalling => {
                self.assigned_traffic_slots = ca.ts_assigned;
            }
        }
        tracing::info!(
            "rx_mac_resource: following same-carrier CHANNEL ALLOCATION {} on carrier {}, assigned traffic slots {:?}",
            ca.alloc_type,
            ca.carrier_num,
            self.assigned_traffic_slots
        );
    }

    /// Relay decoded downlink circuit-mode (TCH/S) speech up to CMCE.
    ///
    /// The LMAC decodes each downlink traffic burst (TCH/S) and delivers it over
    /// the TMD-SAP (cl. 23) tagged with the timeslot it arrived on. On the MS the
    /// U-plane switch and call state live in CMCE (CC-MS, cl. 14.5.1.4), so the
    /// MAC simply forwards the frame upward; it performs no audio processing.
    ///
    /// As a MAC-level guard it relays only on a timeslot the network has
    /// assigned to us as a traffic channel via a same-carrier CHANNEL ALLOCATION
    /// element (cl. 21.5.2, recorded in `assigned_traffic_slots`). The assigned
    /// slot is followed from the element — a same-carrier TCH is on some slot
    /// other than the control channel, so this is never hardcoded to TS1. Bursts
    /// on control or other-call timeslots are dropped here. The LMAC still gates
    /// the physical decode on the per-slot AACH traffic marker (cl. 21.4.7.2),
    /// and the definitive U-plane switch gate (is the call actually receiving)
    /// remains in CC-MS.
    fn rx_tmd_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tmd_prim");
        match message.msg {
            SapMsgInner::TmdCircuitDataInd(prim) => {
                let ts = prim.ts;
                let assigned = (1..=4).contains(&ts) && self.assigned_traffic_slots[(ts - 1) as usize];
                if !assigned {
                    tracing::trace!("rx_tmd_prim: ts={} not an assigned traffic timeslot, dropping speech frame", ts);
                    return;
                }
                queue.push_back(SapMsg {
                    sap: Sap::TmdSap,
                    src: TetraEntity::Umac,
                    dest: TetraEntity::Cmce,
                    msg: SapMsgInner::TmdCircuitDataInd(prim),
                });
            }
            SapMsgInner::TmdCircuitDataReq(prim) => {
                // Uplink U-plane source frame from CC-MS (the U-plane owner, cl.
                // 14.5.1.4). UMAC is the transmit timing authority (cl. 23): it
                // buffers the frame here and clocks exactly one out per granted
                // uplink traffic slot in `tick_start`. The `ts` field is not used
                // for slot selection — the assigned uplink slot comes from the
                // CHANNEL ALLOCATION record (cl. 21.5.2). Drop-oldest keeps the
                // buffer bounded if CC supplies faster than we emit.
                if self.uplink_audio.len() >= UPLINK_AUDIO_MAX_FRAMES {
                    self.uplink_audio.pop_front();
                }
                self.uplink_audio.push_back(prim.data);
            }
            _ => {
                panic!("UMAC-MS: unexpected message on TMD-SAP: {:?}", message.msg);
            }
        }
    }
}

impl TetraEntityTrait for UmacMs {
    fn entity(&self) -> TetraEntity {
        TetraEntity::Umac
    }

    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::debug!("rx_prim: {:?}", message);
        // tracing::debug!(ts=%message.dltime, "rx_prim: {:?}", message);

        match message.sap {
            Sap::TmvSap => {
                self.rx_tmv_prim(queue, message);
            }

            Sap::TmaSap => {
                self.rx_tma_prim(queue, message);
            }

            Sap::TlmbSap => {
                self.rx_tlmb_prim(queue, message);
            }

            Sap::TlmcSap => {
                self.rx_tlmc_prim(queue, message);
            }

            // U-plane: decoded downlink circuit-mode (TCH/S) speech from the LMAC
            // (cl. 23, TMD-SAP). The MS MAC relays it up to CMCE, which owns the
            // U-plane switch; the MAC itself performs no audio processing.
            Sap::TmdSap => {
                self.rx_tmd_prim(queue, message);
            }

            _ => {
                panic!()
            }
        }
    }

    fn tick_start(&mut self, queue: &mut MessageQueue, _ts: TdmaTime) {
        // The MS free-runs the absolute downlink time between SYNC bursts,
        // advancing one timeslot per received slot. It is re-seeded from each
        // BSCH in `rx_tmv_bsch` (ETSI TS 100 392-2 cl. 7 / 21.4.4.2). The
        // router's `ts` is a relative pacing clock in MS mode, so it is
        // intentionally not used here.
        self.dltime = self.dltime.add_timeslots(1);

        // Drive any in-flight multi-slot reserved uplink transfer: emit the next
        // MAC-FRAG-UL/MAC-END-UL block when its reserved slot (dltime + 2) is
        // reached (cl. 23.4.2.1.2 / 23.5.2). No-op when nothing is reserved.
        self.drive_reserved_tx(queue);

        // Emit the MS uplink TCH/S traffic burst on the granted slot when this
        // MS is the current talker (cl. 23 transmit scheduling / cl. 14.5.1.4
        // U-plane). No-op unless a U-plane transmit grant and an assigned
        // traffic slot are both active.
        self.drive_uplink_traffic(queue);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tetra_config::bluestation::from_toml_str;
    use tetra_saps::tlmc::{TlmcConfigureReq, TlmcValidAddress};
    use tetra_saps::tma::TmaUnitdataReq;
    use tetra_pdus::umac::enums::access_assign_dl_usage::AccessAssignDlUsage;
    use tetra_pdus::umac::enums::access_assign_ul_usage::AccessAssignUlUsage;
    use tetra_pdus::umac::enums::sysinfo_opt_field_flag::SysinfoOptFieldFlag;
    use tetra_pdus::umac::fields::sysinfo_default_def_for_access_code_a::SysinfoDefaultDefForAccessCodeA;
    use tetra_pdus::umac::pdus::access_assign::AccessField;

    /// Minimal valid MS config (mirrors `example_config/config-ms.toml`); the
    /// DL/UL frequencies must match what `cell_info` recomputes.
    const MS_TOML: &str = r#"
config_version = "0.6"
stack_mode = "Ms"

[phy_io]
backend = "SoapySdr"

[phy_io.soapysdr]
tx_freq = 439825000
rx_freq = 430425000
ppm_err = 0
device = "driver=sx"
sample_rate = 600000
rx_antenna = "RX"
tx_antenna = "TX"
rx_gain_lna = 48.0
rx_gain_pga = 8.0
tx_gain_dac = 0.0
tx_gain_mixer = 0.0

[net_info]
mcc = 901
mnc = 9999

[cell_info]
freq_band = 4
main_carrier = 1593
duplex_spacing = 7
custom_duplex_spacing = 9400000
freq_offset = 0
reverse_operation = false
location_area = 1
colour_code = 1

[ms]
issi = 1000001
subscriber_class = 1
attach_groups = []
"#;

    fn ms_umac() -> UmacMs {
        let cfg = from_toml_str(MS_TOML).expect("valid MS test config");
        UmacMs::new(SharedConfig::from_parts(cfg, None))
    }

    /// An ACCESS-ASSIGN granting an ongoing-frame random access opportunity for
    /// access code A on both uplink subslots (single access field, header == 1;
    /// base frame length 0b0010 = ongoing frame, cl. 21.4.7.2 / Table 21.85).
    fn ongoing_a_assign() -> AccessAssign {
        AccessAssign {
            _header: 1,
            dl_usage: AccessAssignDlUsage::CommonControl,
            ul_usage: AccessAssignUlUsage::CommonOnly,
            f1_af1: None,
            f2_af2: None,
            f2_af: Some(AccessField {
                access_code: 0, // code A
                base_frame_len: 0b0010,
            }),
        }
    }

    /// An "assigned only" ACCESS-ASSIGN: an MS on its common control channel
    /// treats this as a reserved slot, i.e. no random access opportunity
    /// (cl. 23.5.1.4.2).
    fn reserved_assign() -> AccessAssign {
        AccessAssign {
            _header: 1,
            dl_usage: AccessAssignDlUsage::CommonControl,
            ul_usage: AccessAssignUlUsage::AssignedOnly,
            f1_af1: None,
            f2_af2: None,
            f2_af: None,
        }
    }

    /// Set up a UMAC with a queued uplink block and an active random access
    /// attempt (IMM == 15 = immediate access), as after `rx_tma_prim`.
    fn umac_with_active_attempt() -> UmacMs {
        let mut umac = ms_umac();
        umac.dltime = TdmaTime { t: 1, f: 1, m: 1, h: 0 };
        umac.scrambling_code = Some(0x1234_5678);

        umac.access_params.update_sysinfo_default_a(&SysinfoDefaultDefForAccessCodeA {
            imm: 15,
            wt: 6,
            nu: 4,
            fl_factor: false,
            ts_ptr: 0,
            min_pdu_prio: 0,
        });

        let mut sdu = BitBuffer::from_bitstr("0110100100011110001011010010");
        let mac_block = UmacMs::build_mac_access_block(umac.own_issi(), &mut sdu).expect("SDU fits");
        umac.pending_uplink = Some(PendingUplink {
            mac_block,
            logical_channel: LogicalChannel::SchHu,
            scrambling_code: 0x1234_5678,
            tx_reporter: None,
            pdu_priority: None,
            is_emergency: false,
        });

        let p = umac.access_params.params_for(AccessCode::A).expect("params present").clone();
        umac.random_access
            .initiate(umac.dltime, AccessCode::A, &p, 0, false)
            .expect("initiate succeeds");
        assert!(umac.random_access.is_active());
        umac
    }

    /// Set up a UMAC camped with access params (advertising a minimum PDU
    /// priority) and a queued uplink of the given L3 priority/emergency, but with
    /// random access NOT yet initiated — so `drive_random_access` runs the
    /// cl. 23.5.1.4.4 permission gate against the queued priority.
    fn umac_with_queued_priority(min_pdu_prio: u8, pdu_priority: Option<u8>, is_emergency: bool) -> UmacMs {
        let mut umac = ms_umac();
        umac.dltime = TdmaTime { t: 1, f: 1, m: 1, h: 0 };
        umac.scrambling_code = Some(0x1234_5678);
        umac.access_params.update_sysinfo_default_a(&SysinfoDefaultDefForAccessCodeA {
            imm: 15,
            wt: 6,
            nu: 4,
            fl_factor: false,
            ts_ptr: 0,
            min_pdu_prio,
        });
        let mut sdu = BitBuffer::from_bitstr("0110100100011110001011010010");
        let mac_block = UmacMs::build_mac_access_block(umac.own_issi(), &mut sdu).expect("SDU fits");
        umac.pending_uplink = Some(PendingUplink {
            mac_block,
            logical_channel: LogicalChannel::SchHu,
            scrambling_code: 0x1234_5678,
            tx_reporter: None,
            pdu_priority,
            is_emergency,
        });
        umac
    }

    /// Feature 2: `drive_random_access` feeds the queued L3 PDU priority to the
    /// permission gate (cl. 23.5.1.4.4). A non-emergency uplink whose priority is
    /// below the access code's advertised minimum is not permitted, so the
    /// attempt is not started and the block is dropped (the LLC retransmits).
    #[test]
    fn test_below_min_priority_uplink_rejected() {
        let mut umac = umac_with_queued_priority(5, Some(3), false);
        let mut q = MessageQueue::new();

        umac.drive_random_access(&mut q, &ongoing_a_assign());

        assert!(!umac.random_access.is_active(), "gate must reject below-min priority");
        assert!(umac.pending_uplink.is_none(), "rejected uplink is dropped");
        assert!(q.pop_front().is_none(), "no uplink emitted");
    }

    /// Feature 2: an emergency uplink on access code A bypasses the priority gate
    /// (cl. 23.5.1.4.4), so even a below-minimum priority starts the attempt.
    #[test]
    fn test_emergency_uplink_bypasses_priority_gate() {
        let mut umac = umac_with_queued_priority(5, Some(3), true);
        let mut q = MessageQueue::new();

        umac.drive_random_access(&mut q, &ongoing_a_assign());

        assert!(umac.random_access.is_active(), "emergency access on code A bypasses the gate");
        assert!(umac.pending_uplink.is_some(), "block retained for transmission");
    }

    /// 3C-d: on a granting ACCESS-ASSIGN, the UMAC emits the queued MAC-ACCESS
    /// block to LMAC as a TMV-UNITDATA request on SCH/HU at DL+2 timeslots.
    #[test]
    fn test_random_access_emits_uplink_on_opportunity() {
        let mut umac = umac_with_active_attempt();
        let mut q = MessageQueue::new();

        umac.drive_random_access(&mut q, &ongoing_a_assign());

        let msg = q.pop_front().expect("an uplink block should be emitted");
        assert!(q.pop_front().is_none(), "exactly one message emitted");
        assert_eq!(msg.dest, TetraEntity::Lmac);

        let SapMsgInner::TmvUnitdataReq(slot) = msg.msg else {
            panic!("expected TmvUnitdataReq");
        };
        assert_eq!(slot.ts, umac.dltime.add_timeslots(2), "uplink is DL + 2 timeslots");
        let blk = slot.blk1.expect("blk1 carries the MAC-ACCESS");
        assert_eq!(blk.logical_channel, LogicalChannel::SchHu);
        assert_eq!(blk.scrambling_code, 0x1234_5678);
        assert_eq!(blk.mac_block.get_len(), SCH_HU_TYPE1_BITS, "92-bit SCH/HU type-1 block");
    }

    /// 3C-d: a reserved (assigned-only) slot is not a random access opportunity,
    /// so nothing is transmitted and the attempt stays active (cl. 23.5.1.4.2).
    #[test]
    fn test_reserved_slot_emits_nothing() {
        let mut umac = umac_with_active_attempt();
        let mut q = MessageQueue::new();

        umac.drive_random_access(&mut q, &reserved_assign());

        assert!(q.pop_front().is_none(), "no uplink on a reserved slot");
        assert!(umac.random_access.is_active(), "attempt still pending");
        assert!(umac.pending_uplink.is_some(), "queued block retained");
    }

    /// 3C-d: with no active attempt, a granting ACCESS-ASSIGN is ignored (the
    /// state machine must have been initiated by an uplink request first).
    #[test]
    fn test_no_attempt_ignores_opportunity() {
        let mut umac = ms_umac();
        umac.dltime = TdmaTime { t: 1, f: 1, m: 1, h: 0 };
        let mut q = MessageQueue::new();

        umac.drive_random_access(&mut q, &ongoing_a_assign());

        assert!(q.pop_front().is_none(), "idle state machine emits nothing");
    }

    // --- Uplink fragmentation grant handling (cl. 23.5.2 / 23.4.2.1.2) ---

    /// Once the BS acknowledges the frag-start with a MAC-RESOURCE granting one
    /// subslot, the UMAC transmits the buffered remainder as a MAC-END-HU on
    /// SCH/HU (DL + 2 timeslots) and clears the pending fragment.
    #[test]
    fn test_grant_emits_mac_end_hu() {
        let mut umac = ms_umac();
        umac.dltime = TdmaTime { t: 1, f: 1, m: 1, h: 0 };
        let reporter = TxReporter::new();
        umac.pending_fragment = Some(UplinkFragment {
            remainder: BitBuffer::from_bitstr(&"1".repeat(40)),
            scrambling_code: 0xABCD_1234,
            end_kind: FragEndKind::MacEndHu,
            tx_reporter: Some(reporter.clone()),
        });
        let mut q = MessageQueue::new();

        let grant = BasicSlotgrant {
            capacity_allocation: BasicSlotgrantCapAlloc::FirstSubslotGranted,
            granting_delay: BasicSlotgrantGrantingDelay::CapAllocAtNextOpportunity,
        };
        umac.emit_fragment_end(&mut q, Some(&grant));

        let msg = q.pop_front().expect("MAC-END-HU should be emitted");
        assert!(q.pop_front().is_none(), "exactly one message emitted");
        assert_eq!(msg.dest, TetraEntity::Lmac);
        let SapMsgInner::TmvUnitdataReq(slot) = msg.msg else {
            panic!("expected TmvUnitdataReq");
        };
        assert_eq!(slot.ts, umac.dltime.add_timeslots(2), "MAC-END-HU at DL + 2 timeslots");
        assert!(slot.reserved_access, "MAC-END-HU is a reserved-access burst");
        let blk = slot.blk1.expect("blk1 carries the MAC-END-HU");
        assert_eq!(blk.logical_channel, LogicalChannel::SchHu);
        assert_eq!(blk.scrambling_code, 0xABCD_1234);
        assert_eq!(blk.mac_block.get_len(), SCH_HU_TYPE1_BITS, "92-bit SCH/HU type-1 block");
        assert!(umac.pending_fragment.is_none(), "fragment consumed");
        // The LLC transmit receipt is marked transmitted so the acknowledged
        // basic link starts its ack-wait/retransmit timer (cl. 22.3.2.3).
        assert!(reporter.is_transmitted(), "MAC-END-HU emission marks the receipt transmitted");
    }

    /// A remainder too large for a MAC-END-HU subslot is completed by a
    /// MAC-END-UL on a granted SCH/F full slot (Normal Uplink Burst): once the
    /// BS acknowledges the frag-start granting one full slot (`Grant1Slot`), the
    /// UMAC transmits the remainder as a 268-bit MAC-END-UL on SCH/F at DL + 2
    /// timeslots as a reserved-access burst (cl. 23.4.2.1.2 / 23.5.2).
    #[test]
    fn test_grant_emits_mac_end_ul_full_slot() {
        let mut umac = ms_umac();
        umac.dltime = TdmaTime { t: 1, f: 1, m: 1, h: 0 };
        let reporter = TxReporter::new();
        // 160-bit remainder: too large for a MAC-END-HU (max ~81 bits), fits a
        // MAC-END-UL full slot (max ~254 bits).
        umac.pending_fragment = Some(UplinkFragment {
            remainder: BitBuffer::from_bitstr(&"1".repeat(160)),
            scrambling_code: 0x0BAD_F00D,
            end_kind: FragEndKind::MacEndUl,
            tx_reporter: Some(reporter.clone()),
        });
        let mut q = MessageQueue::new();

        let grant = BasicSlotgrant {
            capacity_allocation: BasicSlotgrantCapAlloc::Grant1Slot,
            granting_delay: BasicSlotgrantGrantingDelay::CapAllocAtNextOpportunity,
        };
        umac.emit_fragment_end(&mut q, Some(&grant));

        let msg = q.pop_front().expect("MAC-END-UL should be emitted");
        assert!(q.pop_front().is_none(), "exactly one message emitted");
        assert_eq!(msg.dest, TetraEntity::Lmac);
        let SapMsgInner::TmvUnitdataReq(slot) = msg.msg else {
            panic!("expected TmvUnitdataReq");
        };
        assert_eq!(slot.ts, umac.dltime.add_timeslots(2), "MAC-END-UL at DL + 2 timeslots");
        assert!(slot.reserved_access, "MAC-END-UL is a reserved-access burst");
        let blk = slot.blk1.expect("blk1 carries the MAC-END-UL");
        assert_eq!(blk.logical_channel, LogicalChannel::SchF, "full-slot end uses SCH/F");
        assert_eq!(blk.scrambling_code, 0x0BAD_F00D);
        assert_eq!(blk.mac_block.get_len(), SCH_F_TYPE1_BITS, "268-bit SCH/F type-1 block");
        assert!(umac.pending_fragment.is_none(), "fragment consumed");
        assert!(reporter.is_transmitted(), "MAC-END-UL emission marks the receipt transmitted");
    }

    /// A grant whose capacity does not match the requested fragment-end kind
    /// (here a subslot grant when a full slot was requested) cannot complete the
    /// transfer: nothing is transmitted, the fragment is dropped and its receipt
    /// marked discarded so the LLC retransmits (cl. 23.5.2 / 22.3.2.3).
    #[test]
    fn test_mismatched_grant_drops_fragment() {
        let mut umac = ms_umac();
        umac.dltime = TdmaTime { t: 1, f: 1, m: 1, h: 0 };
        let reporter = TxReporter::new();
        umac.pending_fragment = Some(UplinkFragment {
            remainder: BitBuffer::from_bitstr(&"1".repeat(160)),
            scrambling_code: 0,
            end_kind: FragEndKind::MacEndUl,
            tx_reporter: Some(reporter.clone()),
        });
        let mut q = MessageQueue::new();

        let grant = BasicSlotgrant {
            capacity_allocation: BasicSlotgrantCapAlloc::FirstSubslotGranted,
            granting_delay: BasicSlotgrantGrantingDelay::CapAllocAtNextOpportunity,
        };
        umac.emit_fragment_end(&mut q, Some(&grant));

        assert!(q.pop_front().is_none(), "no uplink on a mismatched grant");
        assert!(umac.pending_fragment.is_none(), "fragment consumed even on mismatch");
        assert!(reporter.is_discarded(), "mismatched-grant drop marks the receipt discarded");
    }

    /// A MAC-RESOURCE that carries no slot grant cannot complete the fragmented
    /// transfer: nothing is transmitted and the fragment is dropped. The LLC
    /// transmit receipt is marked discarded so the acknowledged basic link
    /// retransmits the whole transfer (cl. 22.3.2.3).
    #[test]
    fn test_no_grant_drops_fragment() {
        let mut umac = ms_umac();
        let reporter = TxReporter::new();
        umac.pending_fragment = Some(UplinkFragment {
            remainder: BitBuffer::from_bitstr(&"1".repeat(40)),
            scrambling_code: 0,
            end_kind: FragEndKind::MacEndHu,
            tx_reporter: Some(reporter.clone()),
        });
        let mut q = MessageQueue::new();

        umac.emit_fragment_end(&mut q, None);

        assert!(q.pop_front().is_none(), "no uplink without a grant");
        assert!(umac.pending_fragment.is_none(), "fragment consumed even without a grant");
        assert!(reporter.is_discarded(), "no-grant drop marks the receipt discarded");
    }

    // --- Multi-slot uplink fragmentation (cl. 23.4.2.1.2 / 23.5.2) ---

    /// A remainder too large for one full slot is split into MAC-FRAG-UL
    /// continuations plus a terminating MAC-END-UL across the N reserved full
    /// slots the BS granted. Emission is discontinuous: exactly one block is put
    /// on air per reserved slot (the PHY can only transmit at `dltime + 2`), so
    /// the first block goes out when the grant is processed and the rest as each
    /// reserved slot is reached on later downlink ticks.
    #[test]
    fn test_multislot_grant_schedules_and_drives() {
        let mut umac = ms_umac();
        umac.dltime = TdmaTime { t: 1, f: 1, m: 1, h: 0 };
        let reporter = TxReporter::new();
        // 400-bit remainder => 1 MAC-FRAG-UL (264 SDU bits) + MAC-END-UL (136),
        // i.e. total_slots = 2.
        umac.pending_fragment = Some(UplinkFragment {
            remainder: BitBuffer::from_bitstr(&"1".repeat(400)),
            scrambling_code: 0xFEED_BEEF,
            end_kind: FragEndKind::MacFragUl { total_slots: 2 },
            tx_reporter: Some(reporter.clone()),
        });
        let mut q = MessageQueue::new();

        let grant = BasicSlotgrant {
            capacity_allocation: BasicSlotgrantCapAlloc::Grant2Slots,
            granting_delay: BasicSlotgrantGrantingDelay::CapAllocAtNextOpportunity,
        };
        let slot0 = umac.dltime.add_timeslots(2);
        umac.emit_fragment_end(&mut q, Some(&grant));

        // First reserved slot (dltime + 2) is due this tick: exactly one
        // MAC-FRAG-UL emitted; the plan retains the MAC-END-UL.
        let msg = q.pop_front().expect("first MAC-FRAG-UL emitted");
        assert!(q.pop_front().is_none(), "only one block per slot");
        let SapMsgInner::TmvUnitdataReq(slot) = msg.msg else {
            panic!("expected TmvUnitdataReq");
        };
        assert_eq!(slot.ts, slot0, "first block at DL + 2 timeslots");
        assert!(slot.reserved_access, "reserved-access burst");
        let mut blk0 = slot.blk1.expect("blk1").mac_block;
        assert_eq!(blk0.get_len(), SCH_F_TYPE1_BITS, "full-slot SCH/F block");
        let frag_pdu = MacFragUl::from_bitbuf(&mut blk0).expect("MAC-FRAG-UL decodes");
        assert!(!frag_pdu.fill_bits, "continuation fills the slot, no fill bits");
        assert!(umac.pending_fragment.is_none(), "fragment consumed");
        assert!(umac.reserved_tx.is_some(), "plan still holds the MAC-END-UL");
        assert!(!reporter.is_transmitted(), "receipt not marked until the last block");

        // Advance downlink ticks until the second reserved slot (slot0 + 4) is
        // reached; the MAC-END-UL is emitted then and the receipt marked.
        let mut emitted_end = None;
        for _ in 0..4 {
            umac.tick_start(&mut q, TdmaTime::default());
            if let Some(m) = q.pop_front() {
                emitted_end = Some(m);
            }
            assert!(q.pop_front().is_none(), "at most one block per tick");
        }
        let msg = emitted_end.expect("MAC-END-UL emitted on its reserved slot");
        let SapMsgInner::TmvUnitdataReq(slot) = msg.msg else {
            panic!("expected TmvUnitdataReq");
        };
        assert_eq!(slot.ts, slot0.add_timeslots(4), "second block one TDMA frame later");
        assert!(slot.reserved_access);
        let mut blk1 = slot.blk1.expect("blk1").mac_block;
        assert_eq!(blk1.get_len(), SCH_F_TYPE1_BITS);
        let end_pdu = MacEndUl::from_bitbuf(&mut blk1).expect("MAC-END-UL decodes");
        assert!(end_pdu.length_ind.is_some(), "terminating PDU carries the length");
        assert!(umac.reserved_tx.is_none(), "plan complete");
        assert!(reporter.is_transmitted(), "last block marks the receipt transmitted");
    }

    /// A grant smaller than the requested number of full slots cannot carry the
    /// multi-slot transfer: nothing is transmitted, the fragment is dropped and
    /// its receipt marked discarded so the LLC retransmits (cl. 23.5.2).
    #[test]
    fn test_multislot_grant_too_small_drops() {
        let mut umac = ms_umac();
        umac.dltime = TdmaTime { t: 1, f: 1, m: 1, h: 0 };
        let reporter = TxReporter::new();
        umac.pending_fragment = Some(UplinkFragment {
            remainder: BitBuffer::from_bitstr(&"1".repeat(700)),
            scrambling_code: 0,
            end_kind: FragEndKind::MacFragUl { total_slots: 3 },
            tx_reporter: Some(reporter.clone()),
        });
        let mut q = MessageQueue::new();

        let grant = BasicSlotgrant {
            capacity_allocation: BasicSlotgrantCapAlloc::Grant2Slots,
            granting_delay: BasicSlotgrantGrantingDelay::CapAllocAtNextOpportunity,
        };
        umac.emit_fragment_end(&mut q, Some(&grant));

        assert!(q.pop_front().is_none(), "no uplink on an undersized grant");
        assert!(umac.pending_fragment.is_none(), "fragment consumed");
        assert!(umac.reserved_tx.is_none(), "no plan installed");
        assert!(reporter.is_discarded(), "undersized grant marks the receipt discarded");
    }

    /// The reserved-slot walk steps one TDMA frame at a time (same timeslot,
    /// +4 timeslots) and skips the mandatory CLCH slot, mirroring the BS
    /// grant-opportunity stepping (cl. 23.5.2.2.2).
    #[test]
    fn test_reserved_slot_sequence_skips_clch() {
        // At m=1, the CLCH is f=18, t=2 (is_mandatory_clch). Starting the walk
        // at t=2, f=15, m=1 makes the f=18 candidate a CLCH that must be skipped.
        let first = TdmaTime { t: 2, f: 15, m: 1, h: 0 };
        assert!(TdmaTime { t: 2, f: 18, m: 1, h: 0 }.is_mandatory_clch());
        let slots = UmacMs::reserved_slot_sequence(first, 4);
        assert_eq!(slots.len(), 4);
        let fs: Vec<u8> = slots.iter().map(|s| s.f).collect();
        // f=18 (CLCH) is skipped; the walk continues into the next multiframe.
        assert_eq!(fs, vec![15, 16, 17, 1], "CLCH frame 18 skipped");
        assert!(slots.iter().all(|s| !s.is_mandatory_clch()), "no reserved slot is CLCH");
    }

    /// `compute_ul_frag_slots` sizes N = (continuations) + 1 and rejects a
    /// remainder needing more than `MAX_UL_FRAG_SLOTS` full slots.
    #[test]
    fn test_compute_ul_frag_slots() {
        let end = UmacMs::max_end_ul_sdu_bits(); // 254
        let frag = UmacMs::max_frag_ul_sdu_bits(); // 264
        assert_eq!(UmacMs::compute_ul_frag_slots(end + 1), Some(2), "just over one slot => 2 slots");
        assert_eq!(UmacMs::compute_ul_frag_slots(end + frag), Some(2));
        assert_eq!(UmacMs::compute_ul_frag_slots(end + frag + 1), Some(3));
        // Beyond MAX_UL_FRAG_SLOTS the transfer is rejected.
        let too_big = end + frag * MAX_UL_FRAG_SLOTS;
        assert_eq!(UmacMs::compute_ul_frag_slots(too_big), None);
    }

    /// If a reserved slot is missed (the downlink clock advanced past it without
    /// transmitting), the whole multi-slot plan is abandoned and its receipt
    /// discarded so the LLC/MM retransmit (cl. 23.5.2 / 22.3.2.3).
    #[test]
    fn test_missed_reserved_slot_abandons() {
        let mut umac = ms_umac();
        umac.dltime = TdmaTime { t: 1, f: 5, m: 1, h: 0 };
        let reporter = TxReporter::new();
        // Front reserved slot equals the current downlink slot, i.e. dltime + 2
        // (the uplink slot) is already 2 timeslots past it => missed.
        let mut blocks = VecDeque::new();
        blocks.push_back(BitBuffer::new(SCH_F_TYPE1_BITS));
        let mut slots = VecDeque::new();
        slots.push_back(umac.dltime);
        umac.reserved_tx = Some(ReservedTxPlan {
            blocks,
            slots,
            scrambling_code: 0,
            tx_reporter: Some(reporter.clone()),
        });
        let mut q = MessageQueue::new();

        umac.drive_reserved_tx(&mut q);

        assert!(q.pop_front().is_none(), "missed slot emits nothing");
        assert!(umac.reserved_tx.is_none(), "plan abandoned");
        assert!(reporter.is_discarded(), "missed slot marks the receipt discarded");
    }

    // --- Uplink transfer queueing (cl. 23.5.1.4: one transfer at a time) ---

    /// Build a TMA-UNITDATA request carrying `sdu_bits` for the MS's own ISSI
    /// with `reporter` as its transmit receipt, as the LLC hands down to the MAC.
    fn tma_uplink_req(reporter: &TxReporter, sdu_bits: &str) -> SapMsg {
        SapMsg {
            sap: Sap::TmaSap,
            src: TetraEntity::Llc,
            dest: TetraEntity::Umac,
            msg: SapMsgInner::TmaUnitdataReq(TmaUnitdataReq {
                req_handle: 0,
                pdu: BitBuffer::from_bitstr(sdu_bits),
                main_address: TetraAddress::issi(1000001),
                link_id: 0,
                endpoint_id: 0,
                pdu_priority: None,
                is_emergency: false,
                stealing_permission: false,
                subscriber_class: 0,
                air_interface_encryption: None,
                stealing_repeats_flag: None,
                data_category: None,
                chan_alloc: None,
                tx_reporter: Some(reporter.clone()),
            }),
        }
    }

    /// Regression: a second uplink handed down while the first is still in
    /// flight must be queued, not overwrite the active `pending_uplink`. The
    /// original single-slot design silently replaced the in-flight transfer,
    /// orphaning its `TxReporter` (never marked transmitted or discarded) so the
    /// LLC never set `t_umac_done` and the acknowledged basic link wedged
    /// forever (observed as endless "still blocked" when an LLC BL-ACK auto-ack
    /// interleaved with a post-registration group attach).
    #[test]
    fn test_second_uplink_is_queued_not_clobbered() {
        let mut umac = ms_umac();
        umac.scrambling_code = Some(0x1234_5678);
        let mut q = MessageQueue::new();

        let r1 = TxReporter::new();
        let r2 = TxReporter::new();

        // First transfer installs directly into the active slot.
        umac.rx_tma_prim(&mut q, tma_uplink_req(&r1, "0110100100011110001011010010"));
        assert!(umac.pending_uplink.is_some(), "first transfer is active");
        assert!(umac.uplink_queue.is_empty(), "nothing queued yet");

        // Second transfer arrives before the first completes: it must queue.
        umac.rx_tma_prim(&mut q, tma_uplink_req(&r2, "1001011011100001110100101101"));
        assert_eq!(umac.uplink_queue.len(), 1, "second transfer is queued");
        assert!(umac.pending_uplink.is_some(), "first transfer still active");
        // Neither receipt has been touched: the first is still awaiting its MAC
        // acknowledgement, the second has not yet contended for access.
        assert!(!r1.is_transmitted() && !r1.is_discarded(), "first receipt untouched");
        assert!(!r2.is_transmitted() && !r2.is_discarded(), "second receipt untouched");

        // The active slot must still hold the FIRST transfer (its receipt), not
        // the second: marking the active receipt transmitted marks r1, not r2.
        let active_reporter = umac
            .pending_uplink
            .as_ref()
            .unwrap()
            .tx_reporter
            .clone()
            .expect("active transfer carries the first receipt");
        active_reporter.try_mark_transmitted();
        assert!(r1.is_transmitted(), "active slot holds the first transfer");
        assert!(!r2.is_transmitted(), "second transfer is untouched in the queue");

        // Emulate the first transfer completing (MAC-RESOURCE ack took it), then
        // promotion pulls the queued second transfer into the active slot.
        umac.pending_uplink = None;
        assert!(!umac.random_access.is_active());
        umac.promote_next_uplink();
        assert!(umac.uplink_queue.is_empty(), "queue drained");
        let promoted = umac
            .pending_uplink
            .as_ref()
            .expect("second transfer promoted")
            .tx_reporter
            .clone()
            .expect("promoted transfer carries the second receipt");
        promoted.try_mark_transmitted();
        assert!(r2.is_transmitted(), "promoted slot holds the second transfer");
    }

    /// A queued transfer dropped on queue overflow has its receipt marked
    /// discarded so the LLC retransmits it rather than the receipt being leaked.
    #[test]
    fn test_uplink_queue_overflow_discards_oldest() {
        let mut umac = ms_umac();
        umac.scrambling_code = Some(0x1234_5678);
        let mut q = MessageQueue::new();

        // Occupy the active slot so every subsequent transfer queues.
        umac.rx_tma_prim(&mut q, tma_uplink_req(&TxReporter::new(), "0110100100011110"));

        // Fill the queue to capacity; the oldest queued receipt is the canary.
        let oldest = TxReporter::new();
        umac.rx_tma_prim(&mut q, tma_uplink_req(&oldest, "0110100100011110"));
        for _ in 1..MAX_UPLINK_QUEUE {
            umac.rx_tma_prim(&mut q, tma_uplink_req(&TxReporter::new(), "0110100100011110"));
        }
        assert_eq!(umac.uplink_queue.len(), MAX_UPLINK_QUEUE, "queue at capacity");
        assert!(!oldest.is_discarded(), "oldest still queued");

        // One more transfer overflows the queue, evicting the oldest.
        umac.rx_tma_prim(&mut q, tma_uplink_req(&TxReporter::new(), "0110100100011110"));
        assert_eq!(umac.uplink_queue.len(), MAX_UPLINK_QUEUE, "queue stays capped");
        assert!(oldest.is_discarded(), "evicted transfer's receipt marked discarded (LLC retransmits)");
    }

    // --- MS MAC receive filtering (ETSI TS 100 392-2 clause 23 addressing) ---

    /// Build a UMAC for `mode` ("Ms" / "Mon" / "Bs") with the given own ISSI and
    /// attached groups. The `[ms]` section is only emitted for MS mode (it is
    /// required there and rejected otherwise).
    fn umac_cfg(mode: &str, issi: u32, groups: &[u32]) -> UmacMs {
        let groups_str = groups.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
        let ms_section = if mode == "Ms" {
            format!("\n[ms]\nissi = {issi}\nsubscriber_class = 1\nattach_groups = [{groups_str}]\n")
        } else {
            String::new()
        };
        let toml = format!(
            r#"
config_version = "0.6"
stack_mode = "{mode}"

[phy_io]
backend = "SoapySdr"

[phy_io.soapysdr]
tx_freq = 439825000
rx_freq = 430425000
ppm_err = 0
device = "driver=sx"
sample_rate = 600000
rx_antenna = "RX"
tx_antenna = "TX"
rx_gain_lna = 48.0
rx_gain_pga = 8.0
tx_gain_dac = 0.0
tx_gain_mixer = 0.0

[net_info]
mcc = 901
mnc = 9999

[cell_info]
freq_band = 4
main_carrier = 1593
duplex_spacing = 7
custom_duplex_spacing = 9400000
freq_offset = 0
reverse_operation = false
location_area = 1
colour_code = 1
{ms_section}"#
        );
        let cfg = from_toml_str(&toml).expect("valid test config");
        UmacMs::new(SharedConfig::from_parts(cfg, None))
    }

    /// A generic (untyped) MAC-layer SSI address, as produced by the
    /// MAC-RESOURCE parser for both individual and group identities.
    fn ssi_addr(ssi: u32) -> TetraAddress {
        TetraAddress {
            ssi,
            ssi_type: SsiType::Ssi,
        }
    }

    #[test]
    fn test_ms_accepts_own_issi() {
        let umac = umac_cfg("Ms", 1000001, &[]);
        assert!(umac.accept_downlink_address(&ssi_addr(1000001)));
    }

    #[test]
    fn test_ms_drops_foreign_issi() {
        let umac = umac_cfg("Ms", 1000001, &[]);
        // Traffic addressed to another subscriber must be ignored.
        assert!(!umac.accept_downlink_address(&ssi_addr(2200699)));
    }

    #[test]
    fn test_ms_accepts_attached_group_only() {
        let umac = umac_cfg("Ms", 1000001, &[91, 100]);
        assert!(umac.accept_downlink_address(&ssi_addr(91)));
        assert!(umac.accept_downlink_address(&ssi_addr(100)));
        // A group we have not attached to is not for us.
        assert!(!umac.accept_downlink_address(&ssi_addr(92)));
    }

    #[test]
    fn test_ms_accepts_broadcast() {
        let umac = umac_cfg("Ms", 1000001, &[]);
        assert!(umac.accept_downlink_address(&ssi_addr(BROADCAST_SSI)));
    }

    #[test]
    fn test_monitor_accepts_everything() {
        // Monitor mode is passive: it must observe all traffic, unfiltered.
        let umac = umac_cfg("Mon", 0, &[]);
        assert!(umac.accept_downlink_address(&ssi_addr(2200699)));
        assert!(umac.accept_downlink_address(&ssi_addr(1)));
        assert!(umac.accept_downlink_address(&ssi_addr(BROADCAST_SSI)));
    }

    /// G1 (cl. 17.3.2 / 23.4.1.2.1): a TL-CONFIGURE carrying the MS's identities
    /// (from the MLE-IDENTITIES chain) replaces the runtime downlink address
    /// filter, so a newly-attached group is accepted and a dropped one rejected.
    #[test]
    fn test_tlmc_configure_updates_runtime_filter() {
        let mut umac = umac_cfg("Ms", 1000001, &[91]);
        // Seeded from config: own ISSI + group 91 accepted, 220 not.
        assert!(umac.accept_downlink_address(&ssi_addr(91)));
        assert!(!umac.accept_downlink_address(&ssi_addr(220)));

        // MLE reconfigures the filter: detach 91, attach 220.
        let mut q = MessageQueue::new();
        let msg = SapMsg {
            sap: Sap::TlmcSap,
            src: TetraEntity::Mle,
            dest: TetraEntity::Umac,
            msg: SapMsgInner::TlmcConfigureReq(TlmcConfigureReq {
                valid_addresses: Some(TlmcValidAddress {
                    mcc: 901,
                    mnc: 9999,
                    individual_ssi: Some(1000001),
                    group_ssis: Some(vec![220]),
                }),
                ..Default::default()
            }),
        };
        umac.rx_tlmc_configure_req(&mut q, msg);

        assert!(umac.accept_downlink_address(&ssi_addr(1000001)), "own ISSI still accepted");
        assert!(umac.accept_downlink_address(&ssi_addr(220)), "newly attached group accepted");
        assert!(!umac.accept_downlink_address(&ssi_addr(91)), "detached group now rejected");
    }

    /// G1: a scrambling-only TL-CONFIGURE (no SSI members) leaves the runtime
    /// downlink address filter untouched (e.g. at cell selection).
    #[test]
    fn test_tlmc_configure_scrambling_only_preserves_filter() {
        let mut umac = umac_cfg("Ms", 1000001, &[91]);
        let mut q = MessageQueue::new();
        let msg = SapMsg {
            sap: Sap::TlmcSap,
            src: TetraEntity::Mle,
            dest: TetraEntity::Umac,
            msg: SapMsgInner::TlmcConfigureReq(TlmcConfigureReq {
                valid_addresses: Some(TlmcValidAddress {
                    mcc: 901,
                    mnc: 9999,
                    individual_ssi: None,
                    group_ssis: None,
                }),
                ..Default::default()
            }),
        };
        umac.rx_tlmc_configure_req(&mut q, msg);
        assert!(umac.accept_downlink_address(&ssi_addr(91)), "group filter preserved");
        assert!(umac.accept_downlink_address(&ssi_addr(1000001)), "own ISSI preserved");
    }

    /// D-2 (tune plumbing): a TLMC-TUNE from the MLE is forwarded down to LMAC as
    /// a TMV-TUNE carrying the same carrier (UMAC holds no radio state).
    #[test]
    fn test_tlmc_tune_forwarded_to_lmac() {
        let mut umac = umac_cfg("Ms", 1000001, &[]);
        let mut q = MessageQueue::new();
        let msg = SapMsg {
            sap: Sap::TlmcSap,
            src: TetraEntity::Mle,
            dest: TetraEntity::Umac,
            msg: SapMsgInner::TlmcTuneReq(tetra_saps::tlmc::TlmcTuneReq { carrier_hz: 396_000_000 }),
        };
        umac.rx_tlmc_prim(&mut q, msg);

        let out = q.pop_front().expect("a TMV-TUNE must be emitted");
        assert_eq!(out.sap, Sap::TmvSap);
        assert_eq!(out.dest, TetraEntity::Lmac);
        let SapMsgInner::TmvTuneReq(req) = out.msg else {
            panic!("expected TmvTuneReq");
        };
        assert_eq!(req.carrier_hz, 396_000_000);
    }

    /// A SYSINFO carrying the given RF/duplex parameters (only the frequency
    /// fields matter for uplink derivation; the rest are benign zero defaults).
    fn sysinfo_rf(band: u8, carrier: u16, duplex_idx: u8, reverse: bool) -> MacSysinfo {
        MacSysinfo {
            main_carrier: carrier,
            freq_band: band,
            freq_offset_index: 0,
            duplex_spacing: duplex_idx,
            reverse_operation: reverse,
            num_of_csch: 0,
            ms_txpwr_max_cell: 0,
            rxlev_access_min: 0,
            access_parameter: 0,
            radio_dl_timeout: 0,
            cck_id: None,
            hyperframe_number: None,
            option_field: SysinfoOptFieldFlag::EvenMfDefForTsMode,
            ts_common_frames: None,
            default_access_code: None,
            ext_services: None,
        }
    }

    /// An MS UMAC whose programmed duplex table overrides index 7 with a 9.4 MHz
    /// split (matches the serving BS), so a SYSINFO advertising duplex index 7
    /// resolves to a known uplink.
    fn umac_with_duplex_table() -> UmacMs {
        let toml = r#"
config_version = "0.6"
stack_mode = "Ms"

[phy_io]
backend = "SoapySdr"

[phy_io.soapysdr]
tx_freq = 439825000
rx_freq = 430425000
ppm_err = 0
device = "driver=sx"
sample_rate = 600000
rx_antenna = "RX"
tx_antenna = "TX"
rx_gain_lna = 48.0
rx_gain_pga = 8.0
tx_gain_dac = 0.0
tx_gain_mixer = 0.0

[net_info]
mcc = 901
mnc = 9999

[cell_info]
freq_band = 4
main_carrier = 1593
duplex_spacing = 7
custom_duplex_spacing = 9400000
freq_offset = 0
reverse_operation = false
location_area = 1
colour_code = 1

[duplex_table]
overrides = [[7, 9400000]]

[ms]
issi = 1000001
subscriber_class = 1
attach_groups = []
"#;
        let cfg = from_toml_str(toml).expect("valid test config");
        UmacMs::new(SharedConfig::from_parts(cfg, None))
    }

    /// Camp-time uplink derivation: a SYSINFO advertising band 4 / carrier 1593 /
    /// duplex index 7 (resolved via the programmed duplex table to 9.4 MHz)
    /// yields DL 439.825 MHz, UL 430.425 MHz and emits exactly one TMV-TX-TUNE to
    /// LMAC (EN 300 392-2 cl. 18.4.2.2 / 21.4.4).
    #[test]
    fn test_uplink_derived_from_sysinfo() {
        let mut umac = umac_with_duplex_table();
        let mut q = MessageQueue::new();

        umac.maybe_retune_uplink(&mut q, &sysinfo_rf(4, 1593, 7, false));

        let tunes: Vec<u32> = q
            .iter()
            .filter_map(|m| match &m.msg {
                SapMsgInner::TmvTxTuneReq(p) => {
                    assert_eq!(m.dest, TetraEntity::Lmac);
                    assert_eq!(m.sap, Sap::TmvSap);
                    Some(p.carrier_hz)
                }
                _ => None,
            })
            .collect();
        assert_eq!(tunes, vec![430_425_000], "derived uplink carrier");
        assert_eq!(umac.derived_ul_freq, Some(430_425_000));
    }

    /// The retune is issued only when the derived uplink changes: a second,
    /// identical SYSINFO must not re-emit a TMV-TX-TUNE (avoids retune spam on
    /// every SYSINFO broadcast).
    #[test]
    fn test_uplink_derivation_deduplicated() {
        let mut umac = umac_with_duplex_table();
        let mut q1 = MessageQueue::new();
        umac.maybe_retune_uplink(&mut q1, &sysinfo_rf(4, 1593, 7, false));
        assert!(q1.iter().any(|m| matches!(m.msg, SapMsgInner::TmvTxTuneReq(_))));

        let mut q2 = MessageQueue::new();
        umac.maybe_retune_uplink(&mut q2, &sysinfo_rf(4, 1593, 7, false));
        assert!(
            q2.iter().all(|m| !matches!(m.msg, SapMsgInner::TmvTxTuneReq(_))),
            "unchanged uplink must not retune again"
        );
    }

    /// A malformed SYSINFO (out-of-range band/carrier) is ignored rather than
    /// panicking the frequency-derivation math.
    #[test]
    fn test_uplink_derivation_rejects_bad_sysinfo() {
        let mut umac = umac_with_duplex_table();
        let mut q = MessageQueue::new();
        umac.maybe_retune_uplink(&mut q, &sysinfo_rf(15, 4095, 7, false));
        assert!(q.iter().all(|m| !matches!(m.msg, SapMsgInner::TmvTxTuneReq(_))));
        assert_eq!(umac.derived_ul_freq, None, "no uplink derived from bad params");
    }

    /// Build a downlink CHANNEL ALLOCATION element (cl. 21.5.2) for the given
    /// allocation type, assigned-timeslot bitmap, carrier and UL/DL direction.
    fn chan_alloc(alloc_type: ChanAllocType, ts: [bool; 4], carrier_num: u16, ul_dl: UlDlAssignment) -> ChanAllocElement {
        ChanAllocElement {
            alloc_type,
            ts_assigned: ts,
            ul_dl_assigned: ul_dl,
            clch_permission: false,
            cell_change_flag: false,
            carrier_num,
            ext: None,
            mon_pattern: 0,
            frame18_mon_pattern: Some(0),
        }
    }

    fn tmd_speech(ts: u8, data: Vec<u8>) -> SapMsg {
        SapMsg {
            sap: Sap::TmdSap,
            src: TetraEntity::Lmac,
            dest: TetraEntity::Umac,
            msg: SapMsgInner::TmdCircuitDataInd(tetra_saps::tmd::TmdCircuitDataInd { ts, data }),
        }
    }

    /// M2: a same-carrier CHANNEL ALLOCATION assigning a non-control timeslot is
    /// followed — the assigned slot is recorded from the element (cl. 21.5.2),
    /// never hardcoded to the control channel — and it gates the U-plane TMD
    /// relay: speech on the assigned slot relays to CMCE, speech on any other
    /// timeslot is dropped.
    #[test]
    fn test_channel_allocation_same_carrier_follows_assigned_slot() {
        let mut umac = ms_umac();
        umac.serving_carrier_num = Some(1593);

        // Traffic assigned on TS3 (a non-control timeslot) on the serving carrier.
        umac.act_on_channel_allocation(&chan_alloc(
            ChanAllocType::Replace,
            [false, false, true, false],
            1593,
            UlDlAssignment::Both,
        ));
        assert_eq!(umac.assigned_traffic_slots, [false, false, true, false], "follows assigned TS3");

        let mut q = MessageQueue::new();
        umac.rx_tmd_prim(&mut q, tmd_speech(3, vec![1, 2, 3]));
        umac.rx_tmd_prim(&mut q, tmd_speech(1, vec![9, 9, 9]));
        let relayed: Vec<_> = q
            .iter()
            .filter(|m| m.dest == TetraEntity::Cmce && matches!(m.msg, SapMsgInner::TmdCircuitDataInd(_)))
            .collect();
        assert_eq!(relayed.len(), 1, "only assigned-slot (TS3) speech relayed");
        let SapMsgInner::TmdCircuitDataInd(ind) = &relayed[0].msg else {
            unreachable!()
        };
        assert_eq!(ind.ts, 3, "assigned timeslot tag preserved");
        assert_eq!(ind.data, vec![1, 2, 3], "speech payload preserved");
    }

    /// M2: a CHANNEL ALLOCATION on a different carrier is a cross-carrier retune,
    /// deferred to M3 — the MS must NOT act on it (no assigned-slot change, no
    /// retune), so speech on that slot stays gated out.
    #[test]
    fn test_channel_allocation_cross_carrier_deferred() {
        let mut umac = ms_umac();
        umac.serving_carrier_num = Some(1593);

        umac.act_on_channel_allocation(&chan_alloc(
            ChanAllocType::Replace,
            [false, false, true, false],
            1600, // different carrier
            UlDlAssignment::Both,
        ));
        assert_eq!(umac.assigned_traffic_slots, [false; 4], "cross-carrier allocation not followed");

        let mut q = MessageQueue::new();
        umac.rx_tmd_prim(&mut q, tmd_speech(3, vec![1, 2, 3]));
        assert!(
            q.iter().all(|m| !matches!(m.msg, SapMsgInner::TmdCircuitDataInd(_))),
            "no speech relayed for a cross-carrier (unfollowed) allocation"
        );
    }

    /// M2: an "Additional" allocation adds to the assigned set rather than
    /// replacing it (cl. 21.5.2 / 14.8.17a), and an uplink-only assignment does
    /// not change the downlink decode.
    #[test]
    fn test_channel_allocation_additional_and_ul_only() {
        let mut umac = ms_umac();
        umac.serving_carrier_num = Some(1593);

        umac.act_on_channel_allocation(&chan_alloc(
            ChanAllocType::Replace,
            [false, false, true, false],
            1593,
            UlDlAssignment::Both,
        ));
        umac.act_on_channel_allocation(&chan_alloc(
            ChanAllocType::Additional,
            [false, true, false, false],
            1593,
            UlDlAssignment::Dl,
        ));
        assert_eq!(umac.assigned_traffic_slots, [false, true, true, false], "Additional adds TS2 to TS3");

        // An uplink-only assignment leaves the downlink assigned set untouched.
        umac.act_on_channel_allocation(&chan_alloc(
            ChanAllocType::Replace,
            [true, false, false, false],
            1593,
            UlDlAssignment::Ul,
        ));
        assert_eq!(
            umac.assigned_traffic_slots,
            [false, true, true, false],
            "uplink-only allocation does not change DL slots"
        );
    }

    // ─── M4a: uplink TCH/S traffic emission ────────────────────────────────

    /// Extract the emitted uplink TCH/S traffic burst (TMV-UNITDATA to LMAC),
    /// if any.
    fn extract_uplink_tchs(q: &MessageQueue) -> Option<TmvUnitdataReqSlot> {
        q.iter().find_map(|m| match &m.msg {
            SapMsgInner::TmvUnitdataReq(slot)
                if m.dest == TetraEntity::Lmac
                    && slot
                        .blk1
                        .as_ref()
                        .map(|b| b.logical_channel == LogicalChannel::TchS)
                        .unwrap_or(false) =>
            {
                Some(slot.clone())
            }
            _ => None,
        })
    }

    /// An MLE-CONFIGURE U-plane transmit configuration (cl. 17.3.3) forwarded to
    /// UMAC over the TLMC-SAP.
    fn uplane_cfg(switch_u_plane: bool, tx_grant: bool) -> SapMsg {
        SapMsg {
            sap: Sap::TlmcSap,
            src: TetraEntity::Mle,
            dest: TetraEntity::Umac,
            msg: SapMsgInner::TlmcUPlaneConfigureReq(tetra_saps::tlmc::TlmcUPlaneConfigureReq {
                switch_u_plane,
                tx_grant,
            }),
        }
    }

    /// An uplink U-plane source frame from CC-MS (TMD-SAP, cl. 14.5.1.4).
    fn tmd_source(data: Vec<u8>) -> SapMsg {
        SapMsg {
            sap: Sap::TmdSap,
            src: TetraEntity::Cmce,
            dest: TetraEntity::Umac,
            msg: SapMsgInner::TmdCircuitDataReq(tetra_saps::tmd::TmdCircuitDataReq { ts: 0, data }),
        }
    }

    /// A camped MS holding the transmission grant on an assigned traffic slot
    /// emits exactly one TCH/S Normal-Uplink-Burst request on the paired uplink
    /// slot (dltime + 2, cl. 9.3.9), tagged with the traffic physical channel
    /// and clock-driven (not reserved-access). The slot is followed from the
    /// CHANNEL ALLOCATION record (cl. 21.5.2), never hardcoded.
    #[test]
    fn test_ms_uplink_traffic_emitted_on_granted_assigned_slot() {
        let mut umac = ms_umac();
        umac.scrambling_code = Some(0x1234_5678);
        umac.assigned_traffic_slots = [false, false, true, false]; // TS3
        umac.uplink_tx_granted = true;
        umac.dltime = TdmaTime::default(); // t1/f1 -> paired UL t3/f1

        let mut q = MessageQueue::new();
        umac.drive_uplink_traffic(&mut q);

        let slot = extract_uplink_tchs(&q).expect("a TCH/S uplink burst must be emitted");
        assert_eq!(slot.ts.t, 3, "emitted on the assigned uplink slot (dltime + 2)");
        assert_eq!(slot.ts.f, 1);
        assert_eq!(slot.ul_phy_chan, PhysicalChannel::Tp, "traffic physical channel");
        assert!(!slot.reserved_access, "continuous traffic is clock-driven, not reserved-access");
        assert!(slot.blk2.is_none(), "single full-slot traffic block");
        assert!(slot.bbk.is_none(), "no BBK/AACH on the uplink");
        let blk1 = slot.blk1.expect("blk1 present");
        assert_eq!(blk1.logical_channel, LogicalChannel::TchS);
        assert_eq!(blk1.scrambling_code, 0x1234_5678, "serving-cell scrambling code");
        assert_eq!(blk1.mac_block.get_len(), 274, "one 274-bit TCH/S type-1 frame");
    }

    /// With no floor grant the MS is silent even on its assigned traffic slot.
    #[test]
    fn test_ms_uplink_traffic_suppressed_when_not_granted() {
        let mut umac = ms_umac();
        umac.scrambling_code = Some(0x1234_5678);
        umac.assigned_traffic_slots = [false, false, true, false];
        umac.uplink_tx_granted = false;
        umac.dltime = TdmaTime::default();

        let mut q = MessageQueue::new();
        umac.drive_uplink_traffic(&mut q);
        assert!(extract_uplink_tchs(&q).is_none(), "no traffic emitted without a transmit grant");
    }

    /// Granted, but the paired uplink slot is not one assigned to this MS: stay
    /// silent (do not step on another call's / the control timeslot).
    #[test]
    fn test_ms_uplink_traffic_suppressed_on_unassigned_slot() {
        let mut umac = ms_umac();
        umac.scrambling_code = Some(0x1234_5678);
        umac.assigned_traffic_slots = [true, false, false, false]; // TS1 only
        umac.uplink_tx_granted = true;
        umac.dltime = TdmaTime::default(); // paired UL is TS3, not assigned

        let mut q = MessageQueue::new();
        umac.drive_uplink_traffic(&mut q);
        assert!(extract_uplink_tchs(&q).is_none(), "no traffic emitted on an unassigned slot");
    }

    /// Frame 18 is the control frame — no TCH/S traffic is emitted there even on
    /// an assigned, granted slot (cl. 9.5.1c / 23.4.2.1).
    #[test]
    fn test_ms_uplink_traffic_suppressed_on_frame_18() {
        let mut umac = ms_umac();
        umac.scrambling_code = Some(0x1234_5678);
        umac.assigned_traffic_slots = [false, false, true, false];
        umac.uplink_tx_granted = true;
        umac.dltime = TdmaTime { t: 1, f: 18, m: 1, h: 0 }; // paired UL t3/f18

        let mut q = MessageQueue::new();
        umac.drive_uplink_traffic(&mut q);
        assert!(extract_uplink_tchs(&q).is_none(), "no traffic on the control frame (18)");
    }

    /// Granted and assigned, but not yet camped (no scrambling code): cannot form
    /// a decodable burst, so stay silent.
    #[test]
    fn test_ms_uplink_traffic_suppressed_without_scrambling_code() {
        let mut umac = ms_umac();
        umac.scrambling_code = None;
        umac.assigned_traffic_slots = [false, false, true, false];
        umac.uplink_tx_granted = true;
        umac.dltime = TdmaTime::default();

        let mut q = MessageQueue::new();
        umac.drive_uplink_traffic(&mut q);
        assert!(extract_uplink_tchs(&q).is_none(), "no traffic emitted before camping");
    }

    /// The MLE-CONFIGURE seam drives the transmit grant: tx_grant AND
    /// switch_u_plane together arm emission; either off disarms it and flushes
    /// any buffered uplink audio so a later grant cannot emit stale frames.
    #[test]
    fn test_ms_uplane_configure_seam_sets_and_flushes_grant() {
        let mut umac = ms_umac();
        let mut q = MessageQueue::new();

        umac.uplink_audio.push_back(vec![0u8; 35]);
        umac.rx_tlmc_prim(&mut q, uplane_cfg(true, true));
        assert!(umac.uplink_tx_granted, "grant armed when tx_grant && switch_u_plane");

        // Grant to another user (tx_grant true, U-plane still on) is NOT us.
        umac.rx_tlmc_prim(&mut q, uplane_cfg(true, false));
        assert!(!umac.uplink_tx_granted, "not the talker when tx_grant is false");
        assert!(umac.uplink_audio.is_empty(), "de-grant flushes buffered uplink audio");
    }

    /// The uplink U-plane source buffer is bounded (drop-oldest) so a CC push
    /// rate faster than the emit rate cannot grow it without bound (cl. 23).
    #[test]
    fn test_ms_uplink_audio_buffer_bounded() {
        let mut umac = ms_umac();
        let mut q = MessageQueue::new();
        for i in 0..(UPLINK_AUDIO_MAX_FRAMES + 3) {
            umac.rx_tmd_prim(&mut q, tmd_source(vec![i as u8; 35]));
        }
        assert_eq!(
            umac.uplink_audio.len(),
            UPLINK_AUDIO_MAX_FRAMES,
            "buffer capped, oldest frames dropped"
        );
    }

    /// A buffered (non-silence) source frame is clocked out on the granted slot:
    /// the emitted type-1 block carries the supplied audio, and the buffer entry
    /// is consumed.
    #[test]
    fn test_ms_uplink_emits_buffered_source_frame() {
        let mut umac = ms_umac();
        umac.scrambling_code = Some(0x1234_5678);
        umac.assigned_traffic_slots = [false, false, true, false];
        umac.uplink_tx_granted = true;
        umac.dltime = TdmaTime::default();

        let mut q = MessageQueue::new();
        umac.rx_tmd_prim(&mut q, tmd_source(vec![0xFF; 35])); // distinctive, non-silence
        umac.drive_uplink_traffic(&mut q);

        let slot = extract_uplink_tchs(&q).expect("a TCH/S uplink burst must be emitted");
        let mac_block = slot.blk1.expect("blk1 present").mac_block;
        assert_eq!(mac_block.get_len(), 274);
        assert!(
            mac_block.to_bitstr().contains('1'),
            "emitted the buffered (non-silence) source frame, not synthesised silence"
        );
        assert!(umac.uplink_audio.is_empty(), "buffered frame consumed on emit");
    }

    // ─── M4b: FACCH/STCH stealing of associated signalling ─────────────────

    /// Build a TMA-UNITDATA request with a configurable stealing permission and
    /// link, carrying a short floor-control-sized SDU, as the LLC hands a floor
    /// PDU (BL-DATA) down to the MAC.
    fn tma_stealing_req(stealing: bool, link_id: u32, reporter: Option<TxReporter>) -> SapMsg {
        SapMsg {
            sap: Sap::TmaSap,
            src: TetraEntity::Llc,
            dest: TetraEntity::Umac,
            msg: SapMsgInner::TmaUnitdataReq(TmaUnitdataReq {
                req_handle: 0,
                // ~24-bit floor PDU (U-TX-CEASED / U-TX-DEMAND are tiny).
                pdu: BitBuffer::from_bitstr(&"1".repeat(24)),
                main_address: TetraAddress::issi(1000001),
                link_id,
                endpoint_id: 0,
                pdu_priority: None,
                is_emergency: false,
                stealing_permission: stealing,
                subscriber_class: 0,
                air_interface_encryption: None,
                stealing_repeats_flag: None,
                data_category: None,
                chan_alloc: None,
                tx_reporter: reporter,
            }),
        }
    }

    /// Extract the emitted stolen uplink burst (blk1 = STCH signalling,
    /// blk2 = TCH/S speech half), if any.
    fn extract_stolen_burst(q: &MessageQueue) -> Option<TmvUnitdataReqSlot> {
        q.iter().find_map(|m| match &m.msg {
            SapMsgInner::TmvUnitdataReq(slot)
                if m.dest == TetraEntity::Lmac
                    && slot
                        .blk1
                        .as_ref()
                        .map(|b| b.logical_channel == LogicalChannel::Stch)
                        .unwrap_or(false) =>
            {
                Some(slot.clone())
            }
            _ => None,
        })
    }

    /// When a traffic channel is assigned and L3 requests stealing, the floor PDU
    /// is queued for FACCH/STCH stealing on the TCH — NOT sent by MCCH random
    /// access. `rx_tma_prim` neither emits a burst nor sets a random-access
    /// pending transfer; it parks the block in `pending_stolen_signalling`.
    #[test]
    fn test_ms_stealing_request_queues_on_assigned_tch() {
        let mut umac = ms_umac();
        umac.scrambling_code = Some(0x1234_5678);
        umac.assigned_traffic_slots = [false, false, true, false]; // TS3 assigned

        let mut q = MessageQueue::new();
        umac.rx_tma_prim(&mut q, tma_stealing_req(true, 2, None));

        assert_eq!(umac.pending_stolen_signalling.len(), 1, "floor PDU queued for stealing");
        assert!(umac.pending_uplink.is_none(), "no MCCH random-access transfer started");
        assert_eq!(q.iter().count(), 0, "nothing emitted from rx_tma_prim itself");
    }

    /// Talker stealing: granted + assigned + a queued floor PDU → the granted
    /// slot is stolen. blk1 carries the STCH signalling MAC block (a 124-bit
    /// type-1 block), blk2 the remaining TCH/S speech half; the LMAC will select
    /// normal training sequence 2 from the STCH channel.
    #[test]
    fn test_ms_stealing_emits_stolen_burst_when_talker() {
        let mut umac = ms_umac();
        umac.scrambling_code = Some(0x1234_5678);
        umac.assigned_traffic_slots = [false, false, true, false];
        umac.uplink_tx_granted = true;
        umac.dltime = TdmaTime::default(); // paired UL t3/f1

        let mut q = MessageQueue::new();
        umac.rx_tma_prim(&mut q, tma_stealing_req(true, 2, None));
        umac.drive_uplink_traffic(&mut q);

        let slot = extract_stolen_burst(&q).expect("a stolen STCH burst must be emitted");
        assert_eq!(slot.ts.t, 3, "stolen on the assigned uplink slot");
        assert_eq!(slot.ul_phy_chan, PhysicalChannel::Tp, "traffic physical channel");
        let blk1 = slot.blk1.expect("blk1 (STCH) present");
        assert_eq!(blk1.logical_channel, LogicalChannel::Stch);
        assert_eq!(blk1.mac_block.get_len(), 124, "STCH type-1 half-slot block");
        assert_eq!(blk1.scrambling_code, 0x1234_5678);
        let blk2 = slot.blk2.expect("blk2 (TCH/S speech half) present");
        assert_eq!(blk2.logical_channel, LogicalChannel::TchS);
        assert_eq!(blk2.mac_block.get_len(), 274, "TCH/S speech type-1 frame");
        assert!(umac.pending_stolen_signalling.is_empty(), "queued block consumed");
    }

    /// Listener stealing (U-TX-DEMAND while not the talker): a floor PDU can be
    /// stolen on the assigned uplink slot even without the transmission grant —
    /// the speech half is silence. This is how the MS requests the floor mid-call
    /// over the TCH rather than the MCCH.
    #[test]
    fn test_ms_stealing_emits_stolen_burst_when_listening() {
        let mut umac = ms_umac();
        umac.scrambling_code = Some(0x1234_5678);
        umac.assigned_traffic_slots = [false, false, true, false];
        umac.uplink_tx_granted = false; // listening, not the talker
        umac.dltime = TdmaTime::default();

        let mut q = MessageQueue::new();
        umac.rx_tma_prim(&mut q, tma_stealing_req(true, 2, None));
        umac.drive_uplink_traffic(&mut q);

        let slot = extract_stolen_burst(&q).expect("a stolen STCH burst must be emitted while listening");
        let blk1 = slot.blk1.expect("blk1 (STCH) present");
        assert_eq!(blk1.logical_channel, LogicalChannel::Stch);
        let blk2 = slot.blk2.expect("blk2 (silence speech half) present");
        assert_eq!(blk2.logical_channel, LogicalChannel::TchS);
        assert_eq!(blk2.mac_block.get_len(), 274, "silence TCH/S frame fills the speech half");
    }

    /// The stolen block's LLC transmit receipt is marked transmitted when the
    /// block is actually emitted (cl. 22.3.2.3), so the acknowledged-mode basic
    /// link progresses rather than wedging.
    #[test]
    fn test_ms_stealing_marks_tx_reporter_on_emit() {
        let mut umac = ms_umac();
        umac.scrambling_code = Some(0x1234_5678);
        umac.assigned_traffic_slots = [false, false, true, false];
        umac.uplink_tx_granted = true;
        umac.dltime = TdmaTime::default();

        let reporter = TxReporter::new();
        let mut q = MessageQueue::new();
        umac.rx_tma_prim(&mut q, tma_stealing_req(true, 2, Some(reporter.clone())));
        assert!(!reporter.is_transmitted(), "not yet transmitted while queued");

        umac.drive_uplink_traffic(&mut q);
        assert!(reporter.is_transmitted(), "receipt marked transmitted on emit");
    }

    /// Fallback: with NO traffic channel assigned (pre-TCH floor request), a
    /// stealing request cannot steal a slot, so it goes over the MCCH by random
    /// access (a MAC-ACCESS on SCH/HU) — nothing is queued for stealing.
    #[test]
    fn test_ms_stealing_falls_back_to_mcch_without_tch() {
        let mut umac = ms_umac();
        umac.scrambling_code = Some(0x1234_5678);
        umac.assigned_traffic_slots = [false; 4]; // no TCH assigned

        let mut q = MessageQueue::new();
        umac.rx_tma_prim(&mut q, tma_stealing_req(true, 2, None));

        assert!(umac.pending_stolen_signalling.is_empty(), "nothing to steal without a TCH");
        let pending = umac.pending_uplink.as_ref().expect("MCCH random-access transfer started");
        assert_eq!(pending.logical_channel, LogicalChannel::SchHu, "carried on SCH/HU (MCCH access)");
    }
}
