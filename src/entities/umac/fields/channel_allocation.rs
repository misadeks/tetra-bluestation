
// Clause 21.5.2

use core::fmt;

use crate::common::bitbuffer::BitBuffer;
use crate::common::pdu_parse_error::PduParseErr;

#[derive(Debug, Clone)]
pub struct ChanAllocElement {
    // 2
    pub alloc_type: u8,
    // 4
    /// "Timeslot assigned" (4-bit bitmap, one bit per timeslot).
    ///
    /// ETSI EN 300 392-2 (V2.3.2) clause 23.5.4.3.2 specifies this element as a bit map.
    /// Example: TS2 => 0b0010 (decimal 2), TS4 => 0b1000 (decimal 8).
    ///
    /// NOTE: Older code incorrectly treated this as a 0-based timeslot index (0..3).
    pub ts_assigned: u8,
    // 2
    pub ul_dl_assigned: u8,
    // 1
    pub clch_permission: bool,
    // 1
    pub cell_change_flag: bool,
    // 12
    pub carrier_num: u16,
    // 1
    // pub ext_carrier_num_flag: bool,
    // 4 opt
    pub ext_freq_band: Option<u8>,
    // 2 opt
    pub ext_offset: Option<u8>,
    // 3 opt
    pub ext_duplex_spacing: Option<u8>,
    // 1 opt
    pub ext_reverse_operation: Option<bool>,
    // 2
    pub mon_pattern: u8,
    // 2 opt
    pub frame18_mon_pattern: Option<u8>,
    
    // Below is for extended channel allocation which is unsupported for now
    // // 2 opt
    // pub aug_uldl_ass: Option<u8>,
    // // 3 opt
    // pub aug_bandwidth: Option<u8>,
    // // 3 opt
    // pub aug_mod: Option<u8>,
    // // 3 opt
    // pub QAM: Option<u8>,
    // // 3 opt
    // pub RESERVED: Option<u8>,
    // // 3 opt
    // pub aug_conf_chan_stat: Option<u8>,
    // // 4 opt
    // pub bs_link_imbalance: Option<u8>,
    // // 5 opt
    // pub bs_tx_pow_rel: Option<u8>,
    // // 2 opt
    // pub napping_status: Option<u8>,
    // // 2 opt
    // pub napping_info: Option<u8>,
    // // 4 opt
    // pub RESERVED: Option<u8>,
    // // 1
    // pub cond_a_flag: bool,
    // // 16 opt
    // pub cond_a_elem: Option<u16>,
    // // 1
    // pub cond_b_flag: bool,
    // // 16 opt
    // pub cond_b_elem: Option<u16>,
    // // 1
    // pub further_aug_flag: bool,
}


impl ChanAllocElement {
    pub fn bit_len(&self) -> usize {
        let mut len = 2 + 4 + 2 + 1 + 1 + 12 + 1; // includes ext_carrier_num_flag
        if self.ext_freq_band.is_some() {
            len += 4 + 2 + 3 + 1;
        }
        len += 2; // mon_pattern
        if self.mon_pattern == 0 {
            len += 2; // frame18_mon_pattern
        }
        len
    }

    pub fn from_bitbuf(buf: &mut BitBuffer) -> Result<Self, PduParseErr> {
        let mut s = ChanAllocElement {
            alloc_type: 0,
            ts_assigned: 0,
            ul_dl_assigned: 0,
            clch_permission: false,
            cell_change_flag: false,
            carrier_num: 0,
            // ext_carrier_num_flag: false,
            ext_freq_band: None,
            ext_offset: None,
            ext_duplex_spacing: None,
            ext_reverse_operation: None,
            
            mon_pattern: 0,
            frame18_mon_pattern: None,
        };

        s.alloc_type = buf.read_field(2, "alloc_type")? as u8;
        s.ts_assigned = buf.read_field(4, "ts_assigned")? as u8;
        s.ul_dl_assigned = buf.read_field(2, "ul_dl_assigned")? as u8;
        s.clch_permission = buf.read_field(1, "clch_permission")? != 0;
        s.cell_change_flag = buf.read_field(1, "cell_change_flag")? != 0;
        s.carrier_num = buf.read_field(12, "carrier_num")? as u16;

        let ext_carrier_num_flag = buf.read_field(1, "ext_carrier_num_flag")?;
        if ext_carrier_num_flag == 1 { 
            s.ext_freq_band = Some(buf.read_field(4, "ext_freq_band")? as u8);
            s.ext_offset = Some(buf.read_field(2, "ext_offset")? as u8);
            s.ext_duplex_spacing = Some(buf.read_field(3, "ext_duplex_spacing")? as u8);
            s.ext_reverse_operation = Some(buf.read_field(1, "ext_reverse_operation")? != 0); 
        }

        s.mon_pattern = buf.read_field(2, "mon_pattern")? as u8;
        if s.mon_pattern == 0 { 
            s.frame18_mon_pattern = Some(buf.read_field(2, "frame18_mon_pattern")? as u8); 
        }

        if s.ul_dl_assigned == 0 {
            unimplemented!("Augmented channel allocation is not implemented");
        }

        // // TODO REVIEW: conditional read of aug_uldl_ass
        // if s.ul_dl_assigned == 0 { s.aug_uldl_ass = Some(buf.read_bits(2).unwrap() as u8); }
        // // TODO REVIEW: conditional read of aug_bandwidth
        // if s.ul_dl_assigned == 0 { s.aug_bandwidth = Some(buf.read_bits(3).unwrap() as u8); }
        // // TODO REVIEW: conditional read of aug_mod
        // if s.ul_dl_assigned == 0 { s.aug_mod = Some(buf.read_bits(3).unwrap() as u8); }
        // // TODO REVIEW: conditional read of QAM
        // if s.ul_dl_assigned == 0 { s.QAM = Some(buf.read_bits(3).unwrap() as u8); }
        // // TODO REVIEW: conditional read of RESERVED
        // if s.ul_dl_assigned == 0 { s.RESERVED = Some(buf.read_bits(3).unwrap() as u8); }
        // // TODO REVIEW: conditional read of aug_conf_chan_stat
        // if s.ul_dl_assigned == 0 { s.aug_conf_chan_stat = Some(buf.read_bits(3).unwrap() as u8); }
        // // TODO REVIEW: conditional read of bs_link_imbalance
        // if s.ul_dl_assigned == 0 { s.bs_link_imbalance = Some(buf.read_bits(4).unwrap() as u8); }
        // // TODO REVIEW: conditional read of bs_tx_pow_rel
        // if s.ul_dl_assigned == 0 { s.bs_tx_pow_rel = Some(buf.read_bits(5).unwrap() as u8); }
        // // TODO REVIEW: conditional read of napping_status
        // if s.ul_dl_assigned == 0 { s.napping_status = Some(buf.read_bits(2).unwrap() as u8); }
        // // TODO REVIEW: conditional read of napping_info
        // if s.ul_dl_assigned_zero_and_napstat_1 == 0 { s.napping_info = Some(buf.read_bits(2).unwrap() as u8); }
        // // TODO REVIEW: conditional read of RESERVED
        // if s.ul_dl_assigned == 0 { s.RESERVED = Some(buf.read_bits(4).unwrap() as u8); }
        // s.cond_a_flag = buf.read_bits(1).unwrap() != 0;
        // // TODO REVIEW: conditional read of cond_a_elem
        // if s.cond_a_flag == 1 { s.cond_a_elem = Some(buf.read_bits(16).unwrap() as u16); }
        // s.cond_b_flag = buf.read_bits(1).unwrap() != 0;
        // // TODO REVIEW: conditional read of cond_b_elem
        // if s.cond_b_flag == 1 { s.cond_b_elem = Some(buf.read_bits(16).unwrap() as u16); }
        // s.further_aug_flag = buf.read_bits(1).unwrap() != 0;

        Ok(s)
    }

    pub fn to_bitbuf(&self, buf: &mut BitBuffer) {
        buf.write_bits(self.alloc_type as u64, 2);
        buf.write_bits(self.ts_assigned as u64, 4);
        buf.write_bits(self.ul_dl_assigned as u64, 2);
        buf.write_bits(self.clch_permission as u8 as u64, 1);
        buf.write_bits(self.cell_change_flag as u8 as u64, 1);
        buf.write_bits(self.carrier_num as u64, 12);
        
        // Extended carrier number flag (always present)
        if let Some(ext_freq_band) = self.ext_freq_band { 
            buf.write_bits(1, 1);
            buf.write_bits(ext_freq_band as u64, 4); 
            buf.write_bits(self.ext_offset.unwrap() as u64, 2);
            buf.write_bits(self.ext_duplex_spacing.unwrap() as u64, 3);
            buf.write_bits(self.ext_reverse_operation.unwrap() as u8 as u64, 1);
        } else {
            buf.write_bits(0, 1);
        }

        buf.write_bits(self.mon_pattern as u64, 2);
        if self.mon_pattern == 0 {
            buf.write_bits(self.frame18_mon_pattern.unwrap() as u64, 2);
        }

        if self.ul_dl_assigned == 0 {
            unimplemented!("Augmented channel allocation is not implemented");
        }

        // TODO REVIEW: conditional write of aug_uldl_ass
        // if let Some(v) = self.aug_uldl_ass { buf.write_bits(v as u64, 2); }
        // // TODO REVIEW: conditional write of aug_bandwidth
        // if let Some(v) = self.aug_bandwidth { buf.write_bits(v as u64, 3); }
        // // TODO REVIEW: conditional write of aug_mod
        // if let Some(v) = self.aug_mod { buf.write_bits(v as u64, 3); }
        // // TODO REVIEW: conditional write of QAM
        // if let Some(v) = self.QAM { buf.write_bits(v as u64, 3); }
        // // TODO REVIEW: conditional write of RESERVED
        // if let Some(v) = self.RESERVED { buf.write_bits(v as u64, 3); }
        // // TODO REVIEW: conditional write of aug_conf_chan_stat
        // if let Some(v) = self.aug_conf_chan_stat { buf.write_bits(v as u64, 3); }
        // // TODO REVIEW: conditional write of bs_link_imbalance
        // if let Some(v) = self.bs_link_imbalance { buf.write_bits(v as u64, 4); }
        // // TODO REVIEW: conditional write of bs_tx_pow_rel
        // if let Some(v) = self.bs_tx_pow_rel { buf.write_bits(v as u64, 5); }
        // // TODO REVIEW: conditional write of napping_status
        // if let Some(v) = self.napping_status { buf.write_bits(v as u64, 2); }
        // // TODO REVIEW: conditional write of napping_info
        // if let Some(v) = self.napping_info { buf.write_bits(v as u64, 2); }
        // // TODO REVIEW: conditional write of RESERVED
        // if let Some(v) = self.RESERVED { buf.write_bits(v as u64, 4); }
        // buf.write_bits(self.cond_a_flag as u8 as u64, 1);
        // // TODO REVIEW: conditional write of cond_a_elem
        // if let Some(v) = self.cond_a_elem { buf.write_bits(v as u64, 16); }
        // buf.write_bits(self.cond_b_flag as u8 as u64, 1);
        // // TODO REVIEW: conditional write of cond_b_elem
        // if let Some(v) = self.cond_b_elem { buf.write_bits(v as u64, 16); }
        // buf.write_bits(self.further_aug_flag as u8 as u64, 1);
    }
}

impl fmt::Display for ChanAllocElement{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "ChanAllocElement {{ alloc_type: {} ts_assigned: {} ul_dl_assigned: {} clch_permission: {} cell_change_flag: {} carrier_num: {}",
            self.alloc_type,
            self.ts_assigned,
            self.ul_dl_assigned,
            self.clch_permission,
            self.cell_change_flag,
            self.carrier_num,
        )?;

        if let Some(v) = self.ext_freq_band { 
            write!(f, "  freq_band: {}", v)?; 
        }
        if let Some(v) = self.ext_offset { 
            write!(f, "  offset: {}", v)?; 
        }
        if let Some(v) = self.ext_duplex_spacing { 
            write!(f, "  duplex_spacing: {}", v)?; 
        }
        if let Some(v) = self.ext_reverse_operation { 
            write!(f, "  reverse_operation: {}", v)?; 
        }
        write!(f, " mon_pattern: {}", self.mon_pattern)?;
        if let Some(v) = self.frame18_mon_pattern { 
            write!(f, "  frame18_mon_pattern: {}", v)?; 
        }
        write!(f, " }}")
    }
}
