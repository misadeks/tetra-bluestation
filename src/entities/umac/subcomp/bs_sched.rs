use crate::common::etsi_codec;
use crate::common::voice_audio;
use crate::common::voice_audio_io;
use crate::common::voice_state;
use crate::entities::umac::subcomp::fillbits::write_fill_bits;
use crate::{
    common::{
        address::{SsiType, TetraAddress},
        bitbuffer::BitBuffer,
        tdma_time::TdmaTime,
        tetra_common::Todo,
    },
    entities::{
        lmac::components::scramble::SCRAMB_INIT,
        mle::pdus::{d_mle_sync::DMleSync, d_mle_sysinfo::DMleSysinfo},
        umac::{
            enums::{
                access_assign_dl_usage::AccessAssignDlUsage,
                access_assign_ul_usage::AccessAssignUlUsage,
                basic_slotgrant_cap_alloc::BasicSlotgrantCapAlloc,
                basic_slotgrant_granting_delay::BasicSlotgrantGrantingDelay,
                reservation_requirement::ReservationRequirement,
            },
            fields::{basic_slotgrant::BasicSlotgrant, channel_allocation::ChanAllocElement},
            pdus::{
                access_assign::{AccessAssign, AccessField},
                access_assign_fr18::AccessAssignFr18,
                mac_resource::MacResource,
                mac_sync::MacSync,
                mac_sysinfo::MacSysinfo,
            },
        },
    },
    saps::tmv::{
        enums::logical_chans::LogicalChannel,
        {TmvUnitdataReq, TmvUnitdataReqSlot},
    },
    unimplemented_log,
};
use std::collections::{HashMap, VecDeque};

use super::frag::DlFragger;

/// We submit this many TX timeslots ahead of the current time
pub const MACSCHED_TX_AHEAD: usize = 1;

// We schedule up to this many frames ahead
pub const MACSCHED_NUM_FRAMES: usize = 18;

const NULL_PDU_LEN_BITS: usize = 16;
const SCH_HD_CAP: usize = 124;
const SCH_F_CAP: usize = 268;
const TCH_TYPE1_BITS: usize = etsi_codec::TCH_TYPE1_BITS;
const TCH_ENCODED_BITS: usize = etsi_codec::TCH_ENCODED_BITS;
const TRAFFIC_JITTER_TARGET: usize = 3;
const TRAFFIC_JITTER_MAX: usize = 6;
const TCH_START_DELAY_FRAMES: i32 = 4;

#[derive(Debug, Clone)]
struct TrafficFrame {
    time: TdmaTime,
    bits: BitBuffer,
    crc_ok: bool,
}

#[derive(Debug)]
struct TrafficJitterBuffer {
    target: usize,
    max: usize,
    queue: VecDeque<TrafficFrame>,
}

#[derive(Debug, Clone)]
struct TchReservation {
    call_id: u16,
    owner_issi: u32,
    tch_ts: u8,
    tchan: u8,
    carrier: u16,
    start_time: TdmaTime,
    active: bool,
}

impl TrafficJitterBuffer {
    fn new(target: usize, max: usize) -> Self {
        Self {
            target,
            max,
            queue: VecDeque::new(),
        }
    }

    fn push(&mut self, frame: TrafficFrame) {
        if self.queue.len() >= self.max {
            let _ = self.queue.pop_front();
        }
        self.queue.push_back(frame);
    }

    fn pop_ready(&mut self) -> Option<TrafficFrame> {
        if self.queue.len() >= self.target {
            self.queue.pop_front()
        } else {
            None
        }
    }

    fn len(&self) -> usize {
        self.queue.len()
    }
}

#[derive(Debug)]
pub struct PrecomputedUmacPdus {
    pub mac_sysinfo1: MacSysinfo,
    pub mac_sysinfo2: MacSysinfo,
    pub mle_sysinfo: DMleSysinfo,
    pub mac_sync: MacSync,
    pub mle_sync: DMleSync,
}

#[derive(Debug)]
pub struct TimeslotSchedule {
    pub ul1: Option<u32>,
    pub ul2: Option<u32>,
    pub dl: Option<TmvUnitdataReq>,
}

#[derive(Debug)]
pub struct BsChannelScheduler {
    pub cur_ts: TdmaTime,
    scrambling_code: u32,
    precomps: PrecomputedUmacPdus,
    cell_main_carrier: u16,
    pub dltx_queues: [Vec<DlSchedElem>; 4],
    sched: [[TimeslotSchedule; MACSCHED_NUM_FRAMES]; 4],
    traffic_jitter: [TrafficJitterBuffer; 4],
    traffic_last_len_bits: [Option<usize>; 4],
    tch_reservations: HashMap<u8, TchReservation>,
}

#[derive(Debug)]
pub enum DlSchedElem {
    /// A SYSINFO or neighboring cells info block. The integer determines which of the precomputed blocks to use (SYSINFO1, SYSINFO2, NEIGHBORING_CELLS
    Broadcast(Todo),

    /// A received MAC-ACCESS PDU still has to be acknowledged
    RandomAccessAck(TetraAddress),

    /// A slotgrant response, which has to be transmitted with high priority or the delay numbers will be off
    /// ssi and BasicSlotgrant are provided.
    Grant(TetraAddress, BasicSlotgrant),

    /// A MAC-RESOURCE PDU. May be split into fragments upon processing, in which case a FragBuf will be inserted after processing the resource.
    Resource(MacResource, BitBuffer),

    /// A FragBuf containing remaining non-transmitted information after a MAC-RESOURCE start has been transmitted
    FragBuf(DlFragger),
}

const EMPTY_SCHED_ELEM: TimeslotSchedule = TimeslotSchedule {
    ul1: None,
    ul2: None,
    dl: None,
};
const EMPTY_SCHED_CHANNEL: [TimeslotSchedule; MACSCHED_NUM_FRAMES] =
    [EMPTY_SCHED_ELEM; MACSCHED_NUM_FRAMES];
const EMPTY_SCHED: [[TimeslotSchedule; MACSCHED_NUM_FRAMES]; 4] = [EMPTY_SCHED_CHANNEL; 4];

impl BsChannelScheduler {
    pub fn new(
        scrambling_code: u32,
        precomps: PrecomputedUmacPdus,
        cell_main_carrier: u16,
    ) -> Self {
        BsChannelScheduler {
            cur_ts: TdmaTime {
                t: 0,
                f: 0,
                m: 0,
                h: 0,
            }, // Intentionally invalid, updated in tick function
            scrambling_code: scrambling_code,
            precomps,
            cell_main_carrier,
            dltx_queues: [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
            sched: EMPTY_SCHED,
            traffic_jitter: std::array::from_fn(|_| {
                TrafficJitterBuffer::new(TRAFFIC_JITTER_TARGET, TRAFFIC_JITTER_MAX)
            }),
            traffic_last_len_bits: [None, None, None, None],
            tch_reservations: HashMap::new(),
        }
    }

    // pub fn set_scrambling_code(&mut self, scrambling_code: u32) {
    //     self.scrambling_code = scrambling_code;
    //     unimplemented!("need to refresh some msgs possibly");
    // }

    // pub fn set_precomputed_msgs(&mut self, precomps: PrecomputedUmacPdus) {
    //     self.precomps = precomps;
    //     unimplemented!("need to refresh some msgs possibly");
    // }

    /// Fully wipe the schedule
    pub fn purge_schedule(&mut self) {
        self.dltx_queues = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        self.sched = EMPTY_SCHED;
        self.traffic_jitter = std::array::from_fn(|_| {
            TrafficJitterBuffer::new(TRAFFIC_JITTER_TARGET, TRAFFIC_JITTER_MAX)
        });
        self.traffic_last_len_bits = [None, None, None, None];
        self.tch_reservations.clear();
    }

    /// Sets the current downlink time to the given TdmaTime
    /// Wipes the schedule, as it can no longer be guaranteed to be valid
    pub fn set_dl_time(&mut self, new_ts: TdmaTime) {
        self.cur_ts = new_ts;
        self.purge_schedule();
    }

    pub fn ts_to_sched_index(&self, ts: &TdmaTime) -> usize {
        let to_index = (ts.f as usize - 1) + ((ts.m as usize - 1) * 18) + (ts.h as usize * 18 * 60);
        to_index % MACSCHED_NUM_FRAMES
    }

    fn compute_tch_start(&self, ca_ts: TdmaTime, tch_ts: u8) -> TdmaTime {
        let mut start = ca_ts.add_timeslots(4 * TCH_START_DELAY_FRAMES + (tch_ts as i32 - 1));
        if start.f == 18 {
            start = start.add_timeslots(4);
        }
        start
    }

    fn reserve_tch(&mut self, call: &voice_audio::VoiceCall, start_time: TdmaTime) {
        let ts = call.tch_ts;
        let carrier = self.cell_main_carrier;
        let tchan = call.tchan;

        let entry = self.tch_reservations.get_mut(&ts);
        match entry {
            Some(existing) if existing.call_id == call.call_id => {
                // Keep earliest start to avoid pushing traffic later on retries.
                if start_time.diff(existing.start_time) < 0 {
                    existing.start_time = start_time;
                }
            }
            Some(existing) => {
                tracing::warn!(
                    "VOICE TCH reservation: ts {} reassigned from call_id {} to {}",
                    ts,
                    existing.call_id,
                    call.call_id
                );
                *existing = TchReservation {
                    call_id: call.call_id,
                    owner_issi: call.caller_issi,
                    tch_ts: ts,
                    tchan,
                    carrier,
                    start_time,
                    active: false,
                };
            }
            None => {
                self.tch_reservations.insert(
                    ts,
                    TchReservation {
                        call_id: call.call_id,
                        owner_issi: call.caller_issi,
                        tch_ts: ts,
                        tchan,
                        carrier,
                        start_time,
                        active: false,
                    },
                );
            }
        }

        tracing::info!(
            "VOICE TCH reserved: call_id={} owner={} ts={} tchan={} carrier={} start={}",
            call.call_id,
            call.caller_issi,
            ts,
            tchan,
            carrier,
            start_time
        );
    }

    fn cleanup_tch_reservations(&mut self) {
        self.tch_reservations
            .retain(|_ts, res| voice_audio::get_call(res.call_id).is_some());
    }

    fn update_tch_activity(&mut self, ts: TdmaTime) -> Option<TchReservation> {
        let Some(res) = self.tch_reservations.get_mut(&ts.t) else {
            return None;
        };
        if ts.diff(res.start_time) >= 0 && !res.active {
            res.active = true;
            tracing::info!(
                "VOICE TCH active: call_id={} ts={} tchan={} carrier={} start={}",
                res.call_id,
                res.tch_ts,
                res.tchan,
                res.carrier,
                res.start_time
            );
        }
        if res.active { Some(res.clone()) } else { None }
    }

    fn active_tch_for_ts(&self, ts: TdmaTime) -> Option<&TchReservation> {
        let res = self.tch_reservations.get(&ts.t)?;
        if ts.diff(res.start_time) >= 0 {
            Some(res)
        } else {
            None
        }
    }

    ///////// UPLINK GRANT PROCESSING /////////

    /// Finds a grant opportunity for uplink transmission
    /// If num_slots is 1, is_halfslot may specifiy whether only a half slot is needed
    /// Returns (opportunities_to_skip, Vec<timestamps_of_granted_slots>)
    /// Returns None if no suitable opportunity is found in the schedule
    pub fn ul_find_grant_opportunity(
        &self,
        ts: u8,
        num_slots: usize,
        is_halfslot: bool,
    ) -> Option<(usize, Vec<TdmaTime>)> {
        let mut grant_timeslots = Vec::with_capacity(num_slots);
        let mut opportunities_skipped = 0;

        assert!(
            !is_halfslot || num_slots == 1,
            "is_halfslot set for num_slots > 1"
        );

        for dist in 1..MACSCHED_NUM_FRAMES {
            // let candidate_t = self.cur_ts.add_timeslots(dist as i32 * 4);
            // Base off of internal perception of time, convert to UL time
            // Below may crash someday, but I'd want to investigate that situation
            let candidate_t = self.cur_ts.add_timeslots(dist as i32 * 4 - 2);
            assert!(
                candidate_t.t == ts,
                "ul_find_grant_opportunity: candidate_t.ts {} does not match requested ts {}. Please report this to developer. ",
                candidate_t.t,
                ts
            );

            tracing::debug!(
                "ul_find_grant_opportunity: considering candidate ul_ts {}, have {:?}",
                candidate_t,
                grant_timeslots
            );

            if self.cur_ts.is_mandatory_clch() {
                // Not an opportunity; skip
                continue;
            }

            let index = self.ts_to_sched_index(&candidate_t);
            let elem = &self.sched[ts as usize - 1][index];
            // tracing::debug!("ul_find_grant_opportunity: sched[{}] ts {}: {:?}", index, candidate_t, elem);
            if (elem.ul1.is_none() && elem.ul2.is_none())
                || (is_halfslot && (elem.ul1.is_none() || elem.ul2.is_none()))
            {
                // Free UL slot, add this timeslot to result vec
                grant_timeslots.push(candidate_t);
                // continue;
            } else {
                // Something is here, clear our grant timeslots
                opportunities_skipped += grant_timeslots.len() + 1;
                grant_timeslots.clear();
            }

            // Check if done
            if grant_timeslots.len() == num_slots {
                return Some((opportunities_skipped, grant_timeslots));
            }
        }

        // If we get here, we did not find a suitable grant opportunity
        None
    }

    /// Reserves all slots designated in a grant option
    /// If only one halfslot is needed, returns 1 or 2 designating which slot was reserved
    pub fn ul_reserve_grant(
        &mut self,
        ssi: u32,
        grant_timestamps: Vec<TdmaTime>,
        is_halfslot: bool,
    ) -> u8 {
        assert!(!grant_timestamps.is_empty());
        assert!(!is_halfslot || grant_timestamps.len() == 1);
        // let ts = grant_timestamps[0].t as usize;
        for ts in grant_timestamps {
            let index = self.ts_to_sched_index(&ts);

            let elem: &mut TimeslotSchedule = &mut self.sched[ts.t as usize - 1][index];
            if is_halfslot {
                if elem.ul1.is_none() {
                    elem.ul1 = Some(ssi);
                    return 1;
                } else {
                    assert!(
                        elem.ul2.is_none(),
                        "ul_reserve_grant: ul2 already set for ts {:?}, ssi {}",
                        ts,
                        ssi
                    );
                    elem.ul2 = Some(ssi);
                    return 2;
                }
            } else {
                assert!(
                    elem.ul1.is_none(),
                    "ul_reserve_grant: ul1 already set for ts {:?}, ssi {}",
                    ts,
                    ssi
                );
                assert!(
                    elem.ul2.is_none(),
                    "ul_reserve_grant: ul2 already set for ts {:?}, ssi {}",
                    ts,
                    ssi
                );
                elem.ul1 = Some(ssi);
                elem.ul2 = Some(ssi);
            }
        }

        // Full slots reserved
        0
    }

    /// Tries to find a way to satisfy a granting request, and reserves the slots in the schedule.
    /// If successful, returns a BasicSlotgrant with the granting delay and capacity allocation.
    pub fn ul_process_cap_req(
        &mut self,
        timeslot: u8,
        addr: TetraAddress,
        res_req: &ReservationRequirement,
    ) -> Option<BasicSlotgrant> {
        let is_halfslot = res_req == &ReservationRequirement::Req1Subslot;
        let requested_cap = if is_halfslot {
            1
        } else {
            res_req.to_req_slotcount()
        };

        // Find a suitable grant opportunity
        let grant_op = self.ul_find_grant_opportunity(timeslot, requested_cap, is_halfslot);

        tracing::debug!(
            "ul_process_cap_req: addr {}, res_req {:?}, requested_cap {}, is_halfslot {}, grant_op: {:?}",
            addr,
            res_req,
            requested_cap,
            is_halfslot,
            grant_op
        );

        // If found, reserve the slots and return a BasicSlotgrant
        if let Some((skips, grant_timestamps)) = grant_op {
            // Reserve the target granting opportunity. Get subslot (only relevant for halfslot reservation)
            let subslot = self.ul_reserve_grant(addr.ssi, grant_timestamps, is_halfslot);

            // Build BasicSlotgrant response element
            let cap_alloc = if res_req == &ReservationRequirement::Req1Subslot {
                match subslot {
                    1 => BasicSlotgrantCapAlloc::FirstSubslotGranted,
                    2 => BasicSlotgrantCapAlloc::SecondSubslotGranted,
                    _ => unreachable!(
                        "ul_process_cap_req: subslot must be 1 or 2, got {}",
                        subslot
                    ),
                }
            } else {
                BasicSlotgrantCapAlloc::from_req_slotcount(requested_cap)
            };
            let grant_delay = if skips == 0 {
                BasicSlotgrantGrantingDelay::CapAllocAtNextOpportunity
            } else {
                BasicSlotgrantGrantingDelay::DelayNOpportunities(skips as u8)
            };
            Some(BasicSlotgrant {
                capacity_allocation: cap_alloc,
                granting_delay: grant_delay,
            })
        } else {
            tracing::warn!(
                "ul_process_cap_req: no suitable grant opportunity found for addr {}, res_req {:?}",
                addr,
                res_req
            );
            None
        }
    }

    fn ul_get_usage(&self, ts: TdmaTime) -> AccessAssignUlUsage {
        let ul_sched = &self.sched[ts.t as usize - 1][self.ts_to_sched_index(&ts)];
        assert!(ul_sched.ul1.is_some() || ul_sched.ul2.is_none());

        if ul_sched.ul1.is_some() && ul_sched.ul2.is_some() {
            AccessAssignUlUsage::AssignedOnly
        } else if ul_sched.ul1.is_some() {
            AccessAssignUlUsage::CommonAndAssigned
        } else {
            AccessAssignUlUsage::CommonOnly
        }
    }

    ////////// DOWNLINK SCHEDULING /////////

    /// Registers that we should transmit a MAC-RESOURCE or similar with a grant, somewhere this tick
    pub fn dl_enqueue_grant(&mut self, timeslot: u8, addr: TetraAddress, grant: BasicSlotgrant) {
        let ts = if timeslot == 1 {
            1
        } else {
            tracing::warn!(
                "dl_enqueue_grant: TS{} not supported yet; enqueueing on TS1",
                timeslot
            );
            1
        };
        tracing::debug!(
            "dl_enqueue_grant: ts {} enqueueing PDU {:?} for addr {}",
            ts,
            grant,
            addr
        );
        let elem = DlSchedElem::Grant(addr, grant);
        self.dltx_queues[ts as usize - 1].push(elem);
    }

    pub fn dl_enqueue_random_access_ack(&mut self, timeslot: u8, addr: TetraAddress) {
        let ts = if timeslot == 1 {
            1
        } else {
            tracing::warn!(
                "dl_enqueue_random_access_ack: TS{} not supported yet; enqueueing on TS1",
                timeslot
            );

            1
        };

        tracing::debug!(
            "dl_enqueue_random_access_ack: ts {} enqueueing random access acknowledgementfor addr {}",
            ts,
            addr
        );

        let elem = DlSchedElem::RandomAccessAck(addr);

        self.dltx_queues[ts as usize - 1].push(elem);
    }

    pub fn dl_enqueue_tma(&mut self, timeslot: u8, pdu: MacResource, sdu: BitBuffer) {
        let ts = if timeslot == 1 {
            1
        } else {
            tracing::warn!(
                "dl_enqueue_tma: TS{} not supported yet; enqueueing on TS1",
                timeslot
            );

            1
        };

        tracing::debug!(
            "dl_enqueue_tma: ts {} enqueueing PDU {:?} SDU {}",
            ts,
            pdu,
            sdu.dump_bin()
        );

        let elem = DlSchedElem::Resource(pdu, sdu);

        self.dltx_queues[ts as usize - 1].push(elem);
    }

    pub fn dl_schedule_tmb(&mut self, _traffic: BitBuffer, _ts: &TdmaTime) {
        unimplemented!("Broadcast scheduling not implemented yet");
    }

    pub fn dl_schedule_tmd(&mut self, _traffic: BitBuffer, _ts: &TdmaTime) {
        unimplemented!("Traffic scheduling not implemented yet");
    }

    pub fn enqueue_traffic_frame(
        &mut self,
        ts: TdmaTime,
        lchan: LogicalChannel,
        bits: BitBuffer,
        crc_ok: bool,
    ) {
        let idx = (ts.t.saturating_sub(1)) as usize;
        if idx >= self.traffic_jitter.len() {
            tracing::warn!(
                "enqueue_traffic_frame: invalid timeslot {} for {:?}",
                ts.t,
                lchan
            );
            return;
        }
        voice_audio::on_ul_tch(ts.t, ts);
        let len_bits = bits.get_len();
        let len_bytes = (len_bits + 7) / 8;
        tracing::info!(
            "TCH UL buffer: time={} ts={} lchan={:?} len_bits={} len_bytes={} depth={}",
            ts,
            ts.t,
            lchan,
            len_bits,
            len_bytes,
            self.traffic_jitter[idx].len()
        );
        self.traffic_last_len_bits[idx] = Some(len_bits);
        self.traffic_jitter[idx].push(TrafficFrame {
            time: ts,
            bits,
            crc_ok,
        });
    }

    /// Takes a block or None value.
    /// If block is present and some signalling channel, and space is available,
    /// adds a trailing Null PDU.
    /// If blk is None, returns None.
    /// Otherwise, returns blk unchanged (eg. for SYNC, broadcast, etc).
    pub fn try_add_null_pdus(&mut self, blk: Option<TmvUnitdataReq>) -> Option<TmvUnitdataReq> {
        // A null pdu in a slot:
        // 0000000000010000100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000
        // Oddly, the fill_bits ind is set to 0, while a fill bit is indeed present to fill the slot.
        // We replicate that behavior here.
        if let Some(mut b) = blk {
            if b.logical_channel == LogicalChannel::Stch
                || b.logical_channel == LogicalChannel::SchHd
                || b.logical_channel == LogicalChannel::SchF
            {
                if b.mac_block.get_len_remaining() >= NULL_PDU_LEN_BITS {
                    tracing::trace!("try_add_null_pdus: closing blk with Null PDU");

                    // We have room for a Null PDU
                    let mut null_pdu = MacResource::null_pdu();
                    null_pdu.length_ind = 2; // Null PDU is 16 bits
                    let _ = null_pdu.update_len_and_fill_ind(0);
                    null_pdu.to_bitbuf(&mut b.mac_block);
                    // Fill to the end with fill bits (1 then 0s). This improves interoperability
                    // and prevents accidental concatenation of extra PDUs.
                    write_fill_bits(&mut b.mac_block, None);
                } else {
                    tracing::warn!(
                        "try_add_null_pdus: not enough space for Null PDU (need {} bits, have {} bits). Skipping Null PDU.",
                        NULL_PDU_LEN_BITS,
                        b.mac_block.get_len_remaining()
                    );
                }
            }

            Some(b)
        } else {
            None
        }
    }

    fn tch_target_len(&self, _ts: TdmaTime) -> usize {
        if etsi_codec::available() {
            TCH_TYPE1_BITS
        } else {
            TCH_ENCODED_BITS
        }
    }

    fn normalize_tch_bits(&self, ts: TdmaTime, bits: BitBuffer) -> BitBuffer {
        let target_len = self.tch_target_len(ts);
        let src_len = bits.get_len();
        if src_len == target_len {
            return bits;
        }

        let mut src = bits;
        src.seek(0);
        let mut out = BitBuffer::new(target_len);

        if src_len * 2 == target_len {
            out.copy_bits(&mut src, src_len);
            src.seek(0);
            out.copy_bits(&mut src, src_len);
        } else {
            let copy_len = usize::min(src_len, target_len);
            out.copy_bits(&mut src, copy_len);
            if src_len < target_len {
                tracing::warn!(
                    "TCH normalize: padding {} bits (src_len={} target={}) time={}",
                    target_len - src_len,
                    src_len,
                    target_len,
                    ts
                );
            } else {
                tracing::warn!(
                    "TCH normalize: truncating {} bits (src_len={} target={}) time={}",
                    src_len - target_len,
                    src_len,
                    target_len,
                    ts
                );
            }
        }

        out.seek(0);
        out
    }

    fn make_idle_tch_bits(&self, ts: TdmaTime) -> BitBuffer {
        let len_bits = self.tch_target_len(ts);
        let mut buf = BitBuffer::new(len_bits);
        buf.seek(0);
        self.normalize_tch_bits(ts, buf)
    }

    fn try_fill_traffic(&mut self, ts: TdmaTime, elem: &mut TmvUnitdataReqSlot) {
        if ts.f == 18 {
            return;
        }
        let Some(res) = self.active_tch_for_ts(ts).cloned() else {
            return;
        };
        if elem.blk1.is_some() || elem.blk2.is_some() {
            tracing::warn!(
                "TCH override: ts={} blk1={:?} blk2={:?}",
                ts,
                elem.blk1.as_ref().map(|b| b.logical_channel),
                elem.blk2.as_ref().map(|b| b.logical_channel)
            );
            elem.blk1 = None;
            elem.blk2 = None;
        }
        let Some(call) = voice_audio::get_call(res.call_id) else {
            return;
        };
        let Some(call) = voice_audio::get_call(res.call_id) else {
            return;
        };

        let talker_active = call.tx_owner_issi.is_some();
        let local_allowed = etsi_codec::available()
            && match call.mode {
                voice_audio::CallDuplexMode::Duplex => true,
                voice_audio::CallDuplexMode::Simplex => !talker_active,
            };
        let mut local_frame = if local_allowed {
            voice_audio_io::try_take_tx_tch()
        } else {
            None
        };
        let allow_forward = match call.mode {
            voice_audio::CallDuplexMode::Duplex => true,
            voice_audio::CallDuplexMode::Simplex => talker_active || local_frame.is_some(),
        };

        let idx = (ts.t.saturating_sub(1)) as usize;
        let frame = if allow_forward {
            self.traffic_jitter[idx].pop_ready()
        } else {
            None
        };

        let (bits, source) = if let Some(frame) = frame {
            if frame.crc_ok {
                (frame.bits, "forward")
            } else {
                (frame.bits, "forward_bad")
            }
        } else if local_allowed {
            if local_frame.is_none() {
                local_frame = voice_audio_io::try_take_tx_tch();
            }
            if let Some(bits) = local_frame {
                voice_audio::on_ul_tch(ts.t, ts);
                (bits, "local")
            } else {
                (self.make_idle_tch_bits(ts), "idle")
            }
        } else {
            (self.make_idle_tch_bits(ts), "idle")
        };

        let bits = self.normalize_tch_bits(ts, bits);
        let len_bits = bits.get_len();
        let len_bytes = (len_bits + 7) / 8;
        tracing::info!(
            "TCH DL send time={} ts={} mode={:?} talker_active={} src={} len_bits={} len_bytes={} frame_type=unknown jitter_depth={}",
            ts,
            ts.t,
            call.mode,
            talker_active,
            source,
            len_bits,
            len_bytes,
            self.traffic_jitter[idx].len()
        );
        tracing::debug!(
            "TCH burst emitted: call_id={} ts={} tchan={} carrier={} time={}",
            call.call_id,
            res.tch_ts,
            res.tchan,
            res.carrier,
            ts
        );

        elem.blk1 = Some(TmvUnitdataReq {
            logical_channel: LogicalChannel::TchS,
            mac_block: bits,
            scrambling_code: self.scrambling_code,
        });
    }

    fn build_voice_chanalloc_buf(&self, call: &voice_audio::VoiceCall, target_ssi: u32) -> Option<BitBuffer> {
        let addr = TetraAddress {
            encrypted: false,
            ssi_type: SsiType::Ssi,
            ssi: target_ssi,
        };
        let ts_assigned_2bit: u8 = call.tch_ts.saturating_sub(1).min(3);
        if !(1..=4).contains(&call.tch_ts) {
            tracing::warn!(
                "VOICE chanalloc: unexpected tch_ts={} for call_id={}, expected 1..=4",
                call.tch_ts,
                call.call_id
            );
        }
        if self.cell_main_carrier == 0 {
            tracing::warn!(
                "VOICE chanalloc: cell_main_carrier is 0 for call_id={}, check config/sysinfo",
                call.call_id
            );
        }
        let chan_alloc = ChanAllocElement {
            alloc_type: 0,
            ts_assigned: ts_assigned_2bit,
            ul_dl_assigned: 3,
            clch_permission: false,
            cell_change_flag: false,
            carrier_num: self.cell_main_carrier,
            ext_freq_band: None,
            ext_offset: None,
            ext_duplex_spacing: None,
            ext_reverse_operation: None,
            mon_pattern: 0,
            frame18_mon_pattern: Some(0),
        };

        let mut pdu = MacResource {
            fill_bits: false,
            pos_of_grant: 0,
            encryption_mode: 0,
            random_access_flag: false,
            length_ind: 0,
            addr: Some(addr),
            event_label: None,
            usage_marker: Some(call.tchan),
            power_control_element: None,
            slot_granting_element: None,
            chan_alloc_element: Some(chan_alloc),
        };

        let num_fill_bits = pdu.update_len_and_fill_ind(0);
        let mut buf = BitBuffer::new(SCH_F_CAP);
        pdu.to_bitbuf(&mut buf);
        if num_fill_bits > 0 {
            buf.write_bit(1);
            buf.write_zeroes(num_fill_bits - 1);
        }
        buf.seek(0);
        Some(buf)
    }

    /// Returns a mutable reference to the first scheduled resource for the given timeslot and address
    pub fn dl_get_scheduled_resource_for_ssi(
        &mut self,
        ts: TdmaTime,
        addr: &TetraAddress,
    ) -> Option<&mut DlSchedElem> {
        let queue = &mut self.dltx_queues[ts.t as usize - 1];

        for index in 0..queue.len() {
            let elem = &mut queue[index];
            if let DlSchedElem::Resource(pdu, _sdu) = elem {
                if let Some(pdu_ssi) = pdu.addr {
                    if pdu_ssi.ssi == addr.ssi {
                        // Found a resource for this address
                        return queue.get_mut(index);
                    }
                }
            }
        }
        // No resource for this address was found
        None
    }

    /// Make a minimal resource to contain a grant or a random access acknowledgement
    pub fn dl_make_minimal_resource(
        addr: &TetraAddress,
        grant: Option<BasicSlotgrant>,
        random_access_ack: bool,
    ) -> MacResource {
        let mut pdu = MacResource {
            fill_bits: false, // updated later
            pos_of_grant: 0,
            encryption_mode: 0,
            random_access_flag: random_access_ack,
            length_ind: 0, // updated later
            addr: Some(*addr),
            event_label: None,
            usage_marker: None,
            power_control_element: None,
            slot_granting_element: grant,
            chan_alloc_element: None,
        };
        pdu.update_len_and_fill_ind(0);
        pdu
    }

    pub fn dl_take_all_grants_and_acks(&mut self, timeslot: u8) -> Vec<DlSchedElem> {
        let queue = &mut self.dltx_queues[timeslot as usize - 1];
        let mut taken = Vec::new();

        let mut i = 0;
        while i < queue.len() {
            if matches!(
                queue[i],
                DlSchedElem::Grant(_, _) | DlSchedElem::RandomAccessAck(_)
            ) {
                let elem = queue.remove(i);
                taken.push(elem);
            } else {
                i += 1;
            }
        }
        taken
    }

    pub fn dl_integrate_sched_elems_for_timeslot(&mut self, ts: TdmaTime) {
        // Remove all grants and acks from queue and collect them into a vec
        let grants_and_acks = self.dl_take_all_grants_and_acks(ts.t);

        // Process grants and acks
        for elem in grants_and_acks {
            // Try to find existing resource for this address
            let addr = match &elem {
                DlSchedElem::Grant(addr, _) => addr,
                DlSchedElem::RandomAccessAck(addr) => addr,
                _ => panic!(),
            };
            let mac_resource = self.dl_get_scheduled_resource_for_ssi(ts, addr);

            match mac_resource {
                Some(DlSchedElem::Resource(pdu, _sdu)) => {
                    // Integrate grant into the resource
                    match &elem {
                        DlSchedElem::Grant(_, grant) => {
                            tracing::debug!(
                                "dl_integrate_sched_elems_for_timeslot: Integrating grant {:?} into resource for addr {}",
                                grant,
                                addr
                            );
                            pdu.slot_granting_element = Some(grant.clone());
                        }
                        DlSchedElem::RandomAccessAck(_) => {
                            tracing::debug!(
                                "dl_integrate_sched_elems_for_timeslot: Integrating ack into resource for addr {}",
                                addr
                            );
                            pdu.random_access_flag = true;
                        }
                        _ => panic!(),
                    }
                }
                None => {
                    // No resource for this address was found, create a new one

                    let pdu = match &elem {
                        DlSchedElem::Grant(_, grant) => {
                            tracing::debug!(
                                "dl_integrate_sched_elems_for_timeslot: Creating new resource for addr {} with grant {:?}",
                                addr,
                                grant
                            );
                            Self::dl_make_minimal_resource(addr, Some(grant.clone()), false)
                        }
                        DlSchedElem::RandomAccessAck(_) => {
                            tracing::debug!(
                                "dl_integrate_sched_elems_for_timeslot: Creating new resource for addr {} with ack",
                                addr
                            );
                            Self::dl_make_minimal_resource(addr, None, true)
                        }
                        _ => panic!(),
                    };

                    // Push new resource into the queue
                    let dlsched_res = DlSchedElem::Resource(pdu, BitBuffer::new(0));
                    self.dltx_queues[ts.t as usize - 1].push(dlsched_res);
                }
                _ => panic!(),
            }
        }
    }

    /// Return first queued grant.
    /// If none; return first in-progress fragmented message.
    /// If none; return first to-be-transmitted resource.
    /// If none, return None.
    pub fn dl_take_prioritized_sched_item(&mut self, ts: TdmaTime) -> Option<DlSchedElem> {
        if ts.f == 18 {
            // No resources on frame 18
            return None;
        }

        // Map 1-based ts to 0-based index, bail on 0 or out of range.
        let slot = ts.t as usize - 1;
        let q = self.dltx_queues.get_mut(slot).unwrap();

        // Return grants first
        if let Some(i) = q.iter().position(|e| matches!(e, DlSchedElem::Grant(_, _))) {
            return Some(q.remove(i));
        }

        // Return FragBufs next
        if let Some(i) = q.iter().position(|e| matches!(e, DlSchedElem::FragBuf(_))) {
            return Some(q.remove(i));
        }

        // Return Resources last
        if let Some(i) = q
            .iter()
            .position(|e| matches!(e, DlSchedElem::Resource(_, _)))
        {
            return Some(q.remove(i));
        }

        None
    }

    pub fn tick_start(&mut self, ts: TdmaTime) {
        // Increment current time
        self.cur_ts = self.cur_ts.add_timeslots(1);
        assert!(
            ts == self.cur_ts,
            "BsChannelScheduler tick_start: ts mismatch, expected {}, got {}",
            self.cur_ts,
            ts
        );
        self.cleanup_tch_reservations();
        voice_audio::tick(ts);
    }

    /// Prepares a scheduled FUTURE timeslot for transfer to lmac and transmission
    /// Generates BBK block
    /// If the timeslot is not full, generates SYNC SB1/SB2 blocks.
    /// Increments cur_ts by one timeslot.
    /// Caller should check timestamp of returned DlTxElem to prevent desync
    pub fn finalize_ts_for_tick(&mut self) -> TmvUnitdataReqSlot {
        // We finalize a FUTURE slot: cur_ts plus some number of timeslots
        let ts = self.cur_ts.add_timeslots(MACSCHED_TX_AHEAD as i32);
        self.precomps.mac_sync.time = ts;
        self.precomps.mac_sysinfo1.hyperframe_number = Some(ts.h);
        self.precomps.mac_sysinfo2.hyperframe_number = Some(ts.h);
        let tch_active = self.update_tch_activity(ts);
        if tch_active.is_some() {
            tracing::debug!("finalize_ts: mode=TCH time={} ts={}", ts, ts.t);
        } else {
            tracing::trace!("finalize_ts: mode=CONTROL time={} ts={}", ts, ts.t);
        }

        // ---- CHANGE #1: Decide if VOICE chanalloc MUST go out in this TS1 SCH/F
        // We do this BEFORE consuming the normal DL scheduler queue.
        let mut forced_voice_chanalloc: Option<BitBuffer> = None;
        if ts.t == 1 && ts.f != 18 {
            if let Some((call, target_ssi)) = voice_audio::next_chanalloc_due(ts) {
                let start_time = self.compute_tch_start(ts, call.tch_ts);
                self.reserve_tch(&call, start_time);

                if let Some(mut buf) = self.build_voice_chanalloc_buf(&call, target_ssi) {
                    tracing::info!(
                    "VOICE chanalloc: call_id={} addr_ssi={} tch_ts={} ts_assigned={} tchan={} carrier={} start={}",
                    call.call_id,
                    target_ssi,
                    call.tch_ts,
                    call.tch_ts.saturating_sub(1),
                    call.tchan,
                    self.cell_main_carrier,
                    start_time
                );
                    buf.seek(0);
                    forced_voice_chanalloc = Some(buf);
                }
            }
        }


        // TODO FIXME allocate only if we have something to put in it
        let mut buf_opt = BitBuffer::new(SCH_F_CAP);

        if tch_active.is_some() {
            let slot = ts.t as usize - 1;
            if let Some(q) = self.dltx_queues.get_mut(slot) {
                if !q.is_empty() {
                    tracing::warn!(
                        "TCH reservation: dropping {} queued DL items for ts {} at time {}",
                        q.len(),
                        ts.t,
                        ts
                    );
                    q.clear();
                }
            }
        }

        // Integrate all grants and random access acks into resources (either existing or new)
        self.dl_integrate_sched_elems_for_timeslot(ts);

        if forced_voice_chanalloc.is_none() {
            while !self.dltx_queues[ts.t as usize - 1].is_empty() {
                let opt = self.dl_take_prioritized_sched_item(ts);

                if ts.t != 1 && opt.is_some() {
                    tracing::warn!(
                    "dl scheduler: TS{} not supported yet; redirecting pending DL items to TS1",
                    ts.t
                );
                    if let Some(sched_elem) = opt {
                        self.dltx_queues[0].push(sched_elem);
                    }
                    let idx = ts.t as usize - 1;
                    if idx < self.dltx_queues.len() {
                        let moved: Vec<_> = self.dltx_queues[idx].drain(..).collect();
                        self.dltx_queues[0].extend(moved);
                    }
                    break;
                }

                match opt {
                    Some(sched_elem) => match sched_elem {
                        DlSchedElem::Broadcast(_) => {
                            unimplemented_log!(
                            "finalize_ts_for_tick: Broadcast scheduling not implemented"
                        );
                        }

                        DlSchedElem::Resource(pdu, sdu) => {
                            let mut fragger = DlFragger::new(pdu, sdu);
                            let done = fragger.get_next_chunk(SCH_F_CAP, &mut buf_opt);
                            tracing::debug!("<- finalized (chunk) {}", buf_opt.dump_bin());
                            if !done {
                                self.dltx_queues[ts.t as usize - 1]
                                    .insert(0, DlSchedElem::FragBuf(fragger));
                            }
                            break;
                        }

                        DlSchedElem::FragBuf(mut fragger) => {
                            let done = fragger.get_next_chunk(SCH_F_CAP, &mut buf_opt);
                            tracing::debug!("<- finalized (frag/end chunk) {}", buf_opt.dump_bin());
                            if !done {
                                self.dltx_queues[ts.t as usize - 1]
                                    .insert(0, DlSchedElem::FragBuf(fragger));
                            }
                            break;
                        }

                        _ => panic!(
                            "finalize_ts_for_tick: Unexpected DlSchedElem type: {:?}",
                            sched_elem
                        ),
                    },
                    None => break,
                }
            }
        }

        // Check if any signalling message was put
        let mut elem = if buf_opt.get_pos() == 0 {
            // Put default SYNC/SYSINFO frame
            TmvUnitdataReqSlot {
                ts,
                blk1: None,
                blk2: None, // MAY be populated later
                bbk: None,  // WILL be populated later
            }
        } else {
            TmvUnitdataReqSlot {
                ts,
                blk1: Some(TmvUnitdataReq {
                    logical_channel: LogicalChannel::SchF,
                    mac_block: buf_opt,
                    scrambling_code: self.scrambling_code,
                }),
                blk2: None, // MAY be populated later
                bbk: None,  // WILL be populated later
            }
        };

        // ---- CHANGE #3: Force VOICE chanalloc into blk1 even if something else was scheduled.
        if let Some(mut buf) = forced_voice_chanalloc.take() {
            buf.seek(0);
            elem.blk1 = Some(TmvUnitdataReq {
                logical_channel: LogicalChannel::SchF,
                mac_block: buf,
                scrambling_code: self.scrambling_code,
            });
        }

        // Voice: inject channel allocation on SCH/F (TS1) if empty and due
        if ts.t == 1 && ts.f != 18 && elem.blk1.is_none() {
            if let Some((call, target_ssi)) = voice_audio::next_chanalloc_due(ts) {
                let start_time = self.compute_tch_start(ts, call.tch_ts);
                self.reserve_tch(&call, start_time);
                if let Some(mut buf) = self.build_voice_chanalloc_buf(&call, target_ssi) {
                    tracing::info!(
                        "VOICE chanalloc: call_id={} addr_ssi={} tch_ts={} ts_assigned={} tchan={} carrier={} start={}",
                        call.call_id,
                        target_ssi,
                        call.tch_ts,
                        call.tch_ts.saturating_sub(1),
                        call.tchan,
                        self.cell_main_carrier,
                        start_time
                    );
                    buf.seek(0);
                    elem.blk1 = Some(TmvUnitdataReq {
                        logical_channel: LogicalChannel::SchF,
                        mac_block: buf,
                        scrambling_code: self.scrambling_code,
                    });
                }
            }
        }

        // FIXME implement that sched can contain more items, buf for now, fail if that's the case
        // assert!(self.dl_take_schedule_item(&ts).is_none(), "finalize_ts_for_tick: dl_take_schedule_item should return None, but got {:?}", elem);

        // Traffic channel injection (UL->DL repeater)
        self.try_fill_traffic(ts, &mut elem);

        // Voice MVP placeholder injection (optional)
        //
        // This does NOT implement real voice yet. It only reserves one extra timeslot
        // as a temporary "Traffic" slot and transmits STCH blocks there, so we have a
        // safe place to iterate toward single-site voice later.
        let v = voice_state::get();
        if v.enabled
            && ts.f != 18
            && ts.t == v.tch_ts
            && elem.blk1.is_none()
            && elem.blk2.is_none()
            && voice_audio::get_any_call().is_none()
        {
            // Block 1: MAC-RESOURCE with length_ind=0b111110 (2nd block stolen)
            // so the MS may treat Block 2 as STCH as well.
            let mut b1 = BitBuffer::new(124);
            let mut res = MacResource::null_pdu();
            res.length_ind = 0b111110;
            res.to_bitbuf(&mut b1);
            write_fill_bits(&mut b1, None);

            elem.blk1 = Some(TmvUnitdataReq {
                logical_channel: LogicalChannel::Stch,
                mac_block: b1,
                scrambling_code: self.scrambling_code,
            });
            elem.blk2 = Some(TmvUnitdataReq {
                logical_channel: LogicalChannel::Stch,
                mac_block: BitBuffer::new(124),
                scrambling_code: self.scrambling_code,
            });
        }

        // A few sanity checks
        if let Some(ref blk1) = elem.blk1 {
            assert!(ts.f != 18, "frame 18 shouldn't have blk1 set");
            if blk1.logical_channel.is_traffic() {
                tracing::debug!("finalize_ts_for_tick: traffic blk1 scheduled for ts {}", ts);
            }
        }

        // Construct the BBK block to reflect UL/DL usage
        assert!(elem.bbk.is_none(), "BBK block already set");
        elem.bbk = Some(self.generate_bbk_block(ts));

        // tracing::trace!("finalize_ts_for_tick: have {}{}{}",
        //     if elem.bbk.is_some() { "bbk " } else { "" },
        //     if elem.blk1.is_some() { "blk1 " } else { "" },
        //     if elem.blk2.is_some() { "blk2 " } else { "" });

        // Populate blk1 if empty: BSCH on frame 18, SCH/HD on other frames
        if elem.blk1.is_none() {
            elem.blk1 = Some(self.generate_default_blks(ts));
        };

        // Check if second block may still be populated (blk1 is half-slot and blk2 is None)
        let blk1_lchan = elem.blk1.as_ref().unwrap().logical_channel;

        // Populate blk2 with SYSINFO if blk1 is half-slot
        if elem.blk2.is_none()
            && (blk1_lchan == LogicalChannel::Bsch || blk1_lchan == LogicalChannel::SchHd)
        {
            // Check blk1 is indeed short (124 for half-slot or 60 for SYNC)
            assert!(elem.blk1.as_ref().unwrap().mac_block.get_len() <= 124);

            let mut buf = BitBuffer::new(124);

            // Write MAC-SYSINFO (alternating sysinfo1/sysinfo2), followed by MLE-SYSINFO
            if ts.t % 2 == 1 {
                self.precomps.mac_sysinfo1.to_bitbuf(&mut buf);
            } else {
                self.precomps.mac_sysinfo2.to_bitbuf(&mut buf);
            }
            self.precomps.mle_sysinfo.to_bitbuf(&mut buf);

            elem.blk2 = Some(TmvUnitdataReq {
                logical_channel: LogicalChannel::Bnch,
                mac_block: buf,
                scrambling_code: self.scrambling_code,
            })
        }

        if elem.blk2.is_none() {
            // We're done, no blk2 needed. Sanity-check blk1 length for SCH-F vs Traffic.
            let blk1 = elem.blk1.as_ref().unwrap();
            let len = blk1.mac_block.get_len();
            if blk1.logical_channel.is_traffic() {
                let target = self.tch_target_len(ts);
                assert!(len == target, "traffic blk1 length {} != {}", len, target);
            } else {
                assert!(len == SCH_F_CAP, "blk1 length {} != {}", len, SCH_F_CAP);
            }
        }

        assert!(
            elem.bbk.is_some(),
            "BBK block is not set, this should not happen"
        );
        assert!(
            elem.blk1.is_some(),
            "blk1 block is not set, this should not happen"
        );

        // If signalling channels are here, and there is spare room, we need to close them with a Null pdu
        elem.blk1 = self.try_add_null_pdus(elem.blk1);
        elem.blk2 = self.try_add_null_pdus(elem.blk2);

        // Move all BitBuffer positions to the start of the window
        elem.bbk.as_mut().unwrap().mac_block.seek(0);
        elem.blk1.as_mut().unwrap().mac_block.seek(0);
        if let Some(blk2) = elem.blk2.as_mut() {
            blk2.mac_block.seek(0);
        }

        // Clear UL schedule for this timeslot
        let index = self.ts_to_sched_index(&ts);
        self.sched[ts.t as usize - 1][index].ul1 = None;
        self.sched[ts.t as usize - 1][index].ul2 = None;

        // We now have our bbk, blk1 and (optional) blk2
        elem
    }

    fn generate_bbk_block(&self, ts: TdmaTime) -> TmvUnitdataReq {
        // let index = self.ts_to_sched_index(&ts);
        // let sched = &self.sched[ts.t as usize - 1][index];

        // Generate BBK block
        let mut aach_bb = BitBuffer::new(14);
        if ts.f != 18 {
            let mut aach = AccessAssign::default();

            match ts.t {
                1 => {
                    // STRATEGY:
                    // - Send UL AssignedOnly if both ul1 and ul2 has been granted to an MS
                    // - Send UL CommonAndAssigned if only ul1 has been granted
                    // - Send UL CommonOnly if no grants have been made
                    aach.dl_usage = AccessAssignDlUsage::CommonControl;
                    aach.ul_usage = self.ul_get_usage(ts);
                    match aach.ul_usage {
                        AccessAssignUlUsage::CommonOnly => {
                            aach.f1_af1 = Some(AccessField {
                                access_code: 0,
                                base_frame_len: 4,
                            });
                            aach.f2_af2 = Some(AccessField {
                                access_code: 0,
                                base_frame_len: 4,
                            });
                        }
                        AccessAssignUlUsage::CommonAndAssigned
                        | AccessAssignUlUsage::AssignedOnly => {
                            aach.f2_af = Some(AccessField {
                                access_code: 0,
                                base_frame_len: 4,
                            });
                        }
                        _ => {
                            // Traffic or unallocated; no AccessFields
                        }
                    }
                }
                2..=4 => {
                    // Additional channels: traffic reservations take precedence.
                    if let Some(res) = self.active_tch_for_ts(ts) {
                        aach.dl_usage = AccessAssignDlUsage::Traffic(res.tchan);
                        aach.ul_usage = AccessAssignUlUsage::Traffic(res.tchan);
                    } else {
                        // Voice MVP placeholder can advertise ONE configured timeslot as Traffic.
                        let v = voice_state::get();
                        if v.enabled && ts.f != 18 && ts.t == v.tch_ts {
                            aach.dl_usage = AccessAssignDlUsage::Traffic(v.tchan);
                            aach.ul_usage = AccessAssignUlUsage::Traffic(v.tchan);
                        } else {
                            // Those are currently unimplemented, so, unallocated it is
                            aach.dl_usage = AccessAssignDlUsage::Unallocated;
                            aach.ul_usage = AccessAssignUlUsage::Unallocated;
                        }
                    }
                }
                _ => panic!("finalize_ts_for_tick: invalid timeslot {}", ts.t),
            }

            aach.to_bitbuf(&mut aach_bb);
        } else {
            // Fr18
            let aach = AccessAssignFr18 {
                ul_usage: AccessAssignUlUsage::CommonOnly,
                f1_af1: Some(AccessField {
                    access_code: 0,
                    base_frame_len: 1,
                }),
                f2_af2: Some(AccessField {
                    access_code: 0,
                    base_frame_len: 0,
                }),
                ..Default::default()
            };
            // TODO FIXME: Access field defaults are possibly not great
            aach.to_bitbuf(&mut aach_bb);
        }

        TmvUnitdataReq {
            logical_channel: LogicalChannel::Aach,
            mac_block: aach_bb,
            scrambling_code: self.scrambling_code,
        }
    }

    fn generate_default_blks(&self, ts: TdmaTime) -> TmvUnitdataReq {
        match (ts.f, ts.t) {
            (1..=17, 1) => {
                // Two options: [Blk1: Null | Blk2: SYSINFO] or [Both: Null]
                // We'll alternate based on multiframe
                match ts.m % 2 {
                    0 => {
                        // Null + SYSINFO
                        // SYSINFO gets added later, su we just make a half-slot Null pdu here
                        let mut buf1 = BitBuffer::new(SCH_F_CAP);
                        let blk1 = MacResource::null_pdu();
                        blk1.to_bitbuf(&mut buf1);
                        TmvUnitdataReq {
                            logical_channel: LogicalChannel::SchF,
                            mac_block: buf1,
                            scrambling_code: self.scrambling_code,
                        }
                    }
                    1 => {
                        // Full-slot Null pdu
                        let mut buf = BitBuffer::new(SCH_F_CAP);
                        let blk = MacResource::null_pdu();
                        blk.to_bitbuf(&mut buf);
                        TmvUnitdataReq {
                            logical_channel: LogicalChannel::SchF,
                            mac_block: buf,
                            scrambling_code: self.scrambling_code,
                        }
                    }
                    _ => panic!(), // never happens
                }
            }
            (1..=17, 2..=4) | (18, _) => {
                // SYNC + SYSINFO
                let mut buf = BitBuffer::new(60);
                self.precomps.mac_sync.to_bitbuf(&mut buf);
                self.precomps.mle_sync.to_bitbuf(&mut buf);
                TmvUnitdataReq {
                    logical_channel: LogicalChannel::Bsch,
                    mac_block: buf,
                    scrambling_code: SCRAMB_INIT,
                }
            }

            // 1..=17 => {
            //     // Frames 1-17: SCH/HD (NDB burst) with NULL PDU
            //     let mut buf = BitBuffer::new(124);
            //     let mut pdu = MacResource::null_pdu();
            //     let _ = pdu.update_len_and_fill_ind(0);
            //     pdu.to_bitbuf(&mut buf);

            //     (Some(TmvUnitdataReq {
            //         logical_channel: LogicalChannel::SchHd,
            //         mac_block: buf,
            //         scrambling_code: self.scrambling_code,
            //     }), None)
            // },
            // 18 => {
            //     // Frame 18: BSCH (SDB burst) with SYNC
            //     let mut buf = BitBuffer::new(60);
            //     self.precomps.mac_sync.to_bitbuf(&mut buf);
            //     self.precomps.mle_sync.to_bitbuf(&mut buf);

            //     (Some(TmvUnitdataReq {
            //         logical_channel: LogicalChannel::Bsch,
            //         mac_block: buf,
            //         scrambling_code: SCRAMB_INIT,
            //     }), None)
            // },
            _ => panic!(), // never happens
        }
    }

    pub fn dump_ul_schedule(&self, skip_empty: bool) {
        tracing::trace!("Dumping uplink schedule:");
        for dist in 0..MACSCHED_NUM_FRAMES {
            let ts = self.cur_ts.add_timeslots(dist as i32 * 4);
            let index = self.ts_to_sched_index(&ts);
            let elem = &self.sched[ts.t as usize - 1][index];
            if skip_empty && elem.ul1.is_none() && elem.ul2.is_none() && elem.dl.is_none() {
                continue;
            }
            tracing::trace!("  Schedule {}: {:?}", ts, elem);
        }
    }

    pub fn dump_dl_queue(&self) {
        tracing::trace!("Dumping downlink queue:");
        for (index, elem) in self.dltx_queues.iter().enumerate() {
            for e in elem {
                tracing::trace!("  ts[{}] {:?}", index, e);
            }
        }
    }
}

#[cfg(test)]
mod tests {

    use crate::{
        common::{
            address::{SsiType, TetraAddress},
            debug::setup_logging_default,
        },
        entities::{
            mle::fields::bs_service_details::BsServiceDetails,
            umac::{
                enums::sysinfo_opt_field_flag::SysinfoOptFieldFlag,
                fields::{
                    sysinfo_default_def_for_access_code_a::SysinfoDefaultDefForAccessCodeA,
                    sysinfo_ext_services::SysinfoExtendedServices,
                },
            },
        },
    };

    use super::*;

    pub fn get_testing_slotter() -> BsChannelScheduler {
        let _ = setup_logging_default(None);

        // TODO FIXME make all parameters configurable
        let ext_services = SysinfoExtendedServices {
            auth_required: false,
            class1_supported: true,
            class2_supported: true,
            class3_supported: false,
            sck_n: Some(0),
            dck_retrieval_during_cell_select: None,
            dck_retrieval_during_cell_reselect: None,
            linked_gck_crypto_periods: None,
            short_gck_vn: None,
            sdstl_addressing_method: 2,
            gck_supported: false,
            section: 0,
            section_data: 0,
        };

        let def_access = SysinfoDefaultDefForAccessCodeA {
            imm: 8,
            wt: 5,
            nu: 5,
            fl_factor: false,
            ts_ptr: 0,
            min_pdu_prio: 0,
        };

        let sysinfo1 = MacSysinfo {
            main_carrier: 1001,
            freq_band: 4,
            freq_offset_index: 0,
            duplex_spacing: 0,
            reverse_operation: false,
            num_of_csch: 0,
            ms_txpwr_max_cell: 5,
            rxlev_access_min: 3,
            access_parameter: 7,
            radio_dl_timeout: 3,
            cck_id: None,
            hyperframe_number: Some(0),
            option_field: SysinfoOptFieldFlag::DefaultDefForAccCodeA,
            ts_common_frames: None,
            default_access_code: Some(def_access),
            ext_services: None,
        };

        let sysinfo2 = MacSysinfo {
            main_carrier: sysinfo1.main_carrier,
            freq_band: sysinfo1.freq_band,
            freq_offset_index: sysinfo1.freq_offset_index,
            duplex_spacing: sysinfo1.duplex_spacing,
            reverse_operation: sysinfo1.reverse_operation,
            num_of_csch: sysinfo1.num_of_csch,
            ms_txpwr_max_cell: sysinfo1.ms_txpwr_max_cell,
            rxlev_access_min: sysinfo1.rxlev_access_min,
            access_parameter: sysinfo1.access_parameter,
            radio_dl_timeout: sysinfo1.radio_dl_timeout,
            cck_id: sysinfo1.cck_id,
            hyperframe_number: sysinfo1.hyperframe_number,
            option_field: SysinfoOptFieldFlag::ExtServicesBroadcast,
            ts_common_frames: None,
            default_access_code: None,
            ext_services: Some(ext_services),
        };

        let mle_sysinfo_pdu = DMleSysinfo {
            location_area: 2,
            subscriber_class: 65535, // All subscriber classes allowed
            bs_service_details: BsServiceDetails {
                registration: true,
                deregistration: true,
                priority_cell: false,
                no_minimum_mode: true,
                migration: false,
                system_wide_services: false,
                voice_service: true,
                circuit_mode_data_service: false,
                sndcp_service: false,
                aie_service: false,
                advanced_link: false,
            },
        };

        let mac_sync_pdu = MacSync {
            system_code: 1,
            colour_code: 1,
            time: TdmaTime::default(),
            sharing_mode: 0, // Continuous transmission
            ts_reserved_frames: 0,
            u_plane_dtx: false,
            frame_18_ext: false,
        };

        let mle_sync_pdu = DMleSync {
            mcc: 204,
            mnc: 1337,
            neighbor_cell_broadcast: 2,
            cell_load_ca: 0,
            late_entry_supported: true,
        };

        let precomps = PrecomputedUmacPdus {
            mac_sysinfo1: sysinfo1,
            mac_sysinfo2: sysinfo2,
            mle_sysinfo: mle_sysinfo_pdu,
            mac_sync: mac_sync_pdu,
            mle_sync: mle_sync_pdu,
        };

        let mut sched = BsChannelScheduler::new(1, precomps, 0);
        sched.set_dl_time(TdmaTime::default());
        sched
    }

    #[test]
    fn test_halfslot_grants() {
        let mut sched = get_testing_slotter();
        let resreq = ReservationRequirement::Req1Subslot;
        let addr = TetraAddress {
            encrypted: false,
            ssi_type: SsiType::Issi,
            ssi: 1234,
        };
        let grant1 = sched.ul_process_cap_req(1, addr, &resreq);
        tracing::info!("grant1: {:?}", grant1);
        assert!(
            grant1.is_some(),
            "ul_process_cap_req should return Some, but got None"
        );

        let u1 = sched.ul_get_usage(TdmaTime {
            t: 1,
            f: 1,
            m: 1,
            h: 0,
        });
        let u2 = sched.ul_get_usage(TdmaTime {
            t: 1,
            f: 2,
            m: 1,
            h: 0,
        });
        let u3 = sched.ul_get_usage(TdmaTime {
            t: 1,
            f: 3,
            m: 1,
            h: 0,
        });
        tracing::info!("usage ts 1/2/3: {:?}/{:?}/{:?}", u1, u2, u3);

        let cap_alloc1 = grant1.unwrap().capacity_allocation;
        assert_eq!(
            cap_alloc1,
            BasicSlotgrantCapAlloc::FirstSubslotGranted,
            "ul_process_cap_req should return FirstSubslotGranted, but got {:?}",
            cap_alloc1
        );
        let grant2 = sched.ul_process_cap_req(1, addr, &resreq);
        tracing::info!("grant2: {:?}", grant2);
        assert!(
            grant2.is_some(),
            "ul_process_cap_req should return Some, but got None"
        );
        let cap_alloc2 = grant2.unwrap().capacity_allocation;
        assert_eq!(
            cap_alloc2,
            BasicSlotgrantCapAlloc::SecondSubslotGranted,
            "ul_process_cap_req should return SecondSubslotGranted, but got {:?}",
            cap_alloc2
        );

        let u1 = sched.ul_get_usage(TdmaTime {
            t: 1,
            f: 1,
            m: 1,
            h: 0,
        });
        let u2 = sched.ul_get_usage(TdmaTime {
            t: 1,
            f: 2,
            m: 1,
            h: 0,
        });
        let u3 = sched.ul_get_usage(TdmaTime {
            t: 1,
            f: 3,
            m: 1,
            h: 0,
        });
        tracing::info!("usage ts 1/2/3: {:?}/{:?}/{:?}", u1, u2, u3);
    }

    #[test]
    fn test_halfslot_and_fullslot_grant() {
        let mut sched = get_testing_slotter();
        let resreq1 = ReservationRequirement::Req1Subslot;
        let addr = TetraAddress {
            encrypted: false,
            ssi_type: SsiType::Issi,
            ssi: 1234,
        };

        sched.dump_ul_schedule(true);
        let grant1 = sched.ul_process_cap_req(1, addr, &resreq1);
        tracing::info!("grant1: {:?}", grant1);

        let u1 = sched.ul_get_usage(TdmaTime {
            t: 1,
            f: 1,
            m: 1,
            h: 0,
        });
        let u2 = sched.ul_get_usage(TdmaTime {
            t: 1,
            f: 2,
            m: 1,
            h: 0,
        });
        let u3 = sched.ul_get_usage(TdmaTime {
            t: 1,
            f: 3,
            m: 1,
            h: 0,
        });
        tracing::info!("usage ts 1/2/3: {:?}/{:?}/{:?}", u1, u2, u3);

        assert!(grant1.is_some());
        let cap_alloc1 = grant1.unwrap().capacity_allocation;
        assert_eq!(cap_alloc1, BasicSlotgrantCapAlloc::FirstSubslotGranted);

        sched.dump_ul_schedule(true);
        let resreq2 = ReservationRequirement::Req3Slots;
        let Some(grant2) = sched.ul_process_cap_req(1, addr, &resreq2) else {
            panic!()
        };
        tracing::info!("grant2: {:?}", grant2);
        sched.dump_ul_schedule(true);

        let u1 = sched.ul_get_usage(TdmaTime {
            t: 1,
            f: 1,
            m: 1,
            h: 0,
        });
        let u2 = sched.ul_get_usage(TdmaTime {
            t: 1,
            f: 2,
            m: 1,
            h: 0,
        });
        let u3 = sched.ul_get_usage(TdmaTime {
            t: 1,
            f: 3,
            m: 1,
            h: 0,
        });
        tracing::info!("usage ts 1/2/3: {:?}/{:?}/{:?}", u1, u2, u3);

        assert_eq!(
            grant2.capacity_allocation,
            BasicSlotgrantCapAlloc::Grant3Slots
        );
        assert_eq!(
            grant2.granting_delay,
            BasicSlotgrantGrantingDelay::DelayNOpportunities(1)
        );
    }

    #[test]
    fn test_dl_grant_and_ack_integration() {
        let mut sched = get_testing_slotter();
        let ts = TdmaTime::default();
        let addr = TetraAddress {
            encrypted: false,
            ssi_type: SsiType::Issi,
            ssi: 1234,
        };
        let pdu = BsChannelScheduler::dl_make_minimal_resource(&addr, None, false);
        let sdu = BitBuffer::new(0);
        sched.dl_enqueue_tma(ts.t, pdu, sdu);

        let grant = BasicSlotgrant {
            capacity_allocation: BasicSlotgrantCapAlloc::FirstSubslotGranted,
            granting_delay: BasicSlotgrantGrantingDelay::CapAllocAtNextOpportunity,
        };

        sched.dl_enqueue_grant(ts.t, addr, grant);
        sched.dl_enqueue_random_access_ack(ts.t, addr);

        sched.dump_ul_schedule(true);
        sched.dump_dl_queue();

        assert!(sched.dltx_queues[ts.t as usize - 1].len() == 3);

        tracing::info!("Integrating queue");
        sched.dl_integrate_sched_elems_for_timeslot(ts);

        sched.dump_ul_schedule(true);
        sched.dump_dl_queue();

        assert!(sched.dltx_queues[ts.t as usize - 1].len() == 1);
    }
}
