use std::cmp::min;

use crate::{entities::umac::pdus::{mac_end_dl::MacEndDl, mac_frag_dl::MacFragDl, mac_resource::MacResource}};
use crate::common::bitbuffer::BitBuffer;



#[derive(Debug)]
pub struct DlFragger {
    resource: MacResource,
    machdr_is_written: bool,
    sdu: BitBuffer
}

impl DlFragger {
    pub fn new(resource: MacResource, sdu: BitBuffer) -> Self {
        // assert!(sdu.get_pos() == 0, "SDU must be at the start of the buffer");
        DlFragger { 
            resource,
            machdr_is_written: false,
            sdu
        }
    }

    /// Writes MAC-RESOURCE to dest_buf, starting fragmentation if needed. 
    /// Then, writes as many SDU bits as possible. 
    /// Returns true if the entire SDU was consumed, false if the PDU is fragmented
    /// and more chunks are needed.
    pub fn get_resource_chunk(&mut self, slot_cap: usize, dest_buf: &mut BitBuffer) -> bool {
        assert!(self.sdu.get_pos() == 0, "SDU must be at the start of the buffer");
        assert!(!self.machdr_is_written, "MAC header should not be written yet");
        let hdr_len = self.resource.compute_header_len();
        let macres_len = hdr_len + self.sdu.get_len_remaining();
        let macres_len_bytes = (macres_len + 7) / 8;

        // We'll return having written the MAC header
        // TODO FIXME maybe remove this bool and check alltogether
        self.machdr_is_written = true;
        
        if macres_len_bytes * 8 <= slot_cap {
            // Fits in one MAC-RESOURCE
            self.resource.length_ind = macres_len_bytes as u8;
            let num_fill_bits = (8 - (macres_len % 8)) % 8;
            self.resource.fill_bits = num_fill_bits != 0;
            let sdu_bits = self.sdu.get_len_remaining();


            self.resource.to_bitbuf(dest_buf);
            dest_buf.copy_bits(&mut self.sdu, sdu_bits);
            if num_fill_bits > 0 {
                dest_buf.write_bit(1);
                dest_buf.write_zeroes(num_fill_bits - 1); // Write fill bits
            }

            tracing::debug!("Creating MAC-RESOURCE, len: {} (sdu bits: {}), fillbits: {}, pdu: {:?}", 
                macres_len, sdu_bits, num_fill_bits, self.resource);
            tracing::debug!("buffer: {}", dest_buf.dump_bin());

            // We're done with this packet
            true
            
        } else {
            // We need to start fragmentation. No fill bits are needed
            self.resource.length_ind = 0b111111; // Start of fragmentation
            self.resource.fill_bits = false;

            let sdu_bits = slot_cap - hdr_len;

            tracing::debug!("Creating MAC-RESOURCE (fragged), len: {} (sdu bits: {}, remaining: {})", 
                macres_len, sdu_bits, self.sdu.get_len_remaining() - sdu_bits);

            self.resource.to_bitbuf(dest_buf);
            dest_buf.copy_bits(&mut self.sdu, sdu_bits);
            
            // More fragments follow
            false
        }
    }

    /// After MAC-RESOURCE was output using get_first_chunk, call this function to consume
    /// next chunks. Based on capacity, will determine whether to make a MAC-FRAG or
    /// MAC-END. 
    /// Returns true when MAC-END (DL) was created and no further fragments are needed
    /// TODO FIXME: support adding ChanAlloc element in MAC-END
    pub fn get_frag_or_end_chunk(&mut self, slot_cap: usize, dest_buf: &mut BitBuffer) -> bool {
        
        assert!(self.machdr_is_written, "MAC header should be previously written");
        
        // Check if we can fit all in a MAC-END message
        let sdu_bits = self.sdu.get_len_remaining();
        let macend_len_bits = MacEndDl::compute_hdr_len(false, false) + sdu_bits;
        let macend_len_bytes = (macend_len_bits + 7) / 8;

        // tracing::trace!("MAC-END would have length: {} bits, {} bytes, slot capacity: {} bits", 
        //     macend_len_bits, macend_len_bytes, slot_cap);
        
        if macend_len_bytes * 8 <= slot_cap {
            // Fits in single MAC-END
            let num_fill_bits = (8 - (macend_len_bits % 8)) % 8;
            let pdu = MacEndDl {
                fill_bits: num_fill_bits > 0,
                pos_of_grant: 0, 
                length_ind: macend_len_bytes as u8,
                slot_granting_element: None,
                chan_alloc_element: None,
            };

            tracing::debug!("Creating MAC-END with length: {} (sdu bits: {}) slot capacity: {} bits", 
                macend_len_bits, sdu_bits, slot_cap);
        
            // Write MAC-END header followed by TM-SDU
            pdu.to_bitbuf(dest_buf);
            dest_buf.copy_bits(&mut self.sdu, sdu_bits);
            
            // Write fill bits (if needed)
            if num_fill_bits > 0 {
                dest_buf.write_bit(1);
                dest_buf.write_zeroes(num_fill_bits - 1);
            }
            // We're done with this packet
            true

        } else {

            // Need MAC-FRAG, fill slot (or don't fill, if the MAC-END hdr size is the reason we go for MAC-FRAG)
            let macfrag_hdr_len = 4;
            let sdu_bits_in_frag = min(slot_cap - macfrag_hdr_len, sdu_bits);
            let num_fill_bits = slot_cap - macfrag_hdr_len - sdu_bits_in_frag;

            tracing::debug!("Creating MAC-FRAG, sdu bits: {}, remaining {}, slot capacity: {}, fillbits: {}", 
                sdu_bits_in_frag, sdu_bits - sdu_bits_in_frag, slot_cap, num_fill_bits);

            let pdu = MacFragDl {
                fill_bits: num_fill_bits > 0,
            };
            pdu.to_bitbuf(dest_buf);
            dest_buf.copy_bits(&mut self.sdu, sdu_bits_in_frag);

            if num_fill_bits > 0 {
                dest_buf.write_bit(1);
                dest_buf.write_zeroes(num_fill_bits - 1);
            }

            false
        }
    }

    pub fn get_next_chunk(&mut self, slot_cap: usize, dest_buf: &mut BitBuffer) -> bool {
        if !self.machdr_is_written {
            // First chunk, write MAC-RESOURCE
            self.get_resource_chunk(slot_cap, dest_buf)
        } else {
            // Subsequent chunks, write MAC-FRAG or MAC-END
            self.get_frag_or_end_chunk(slot_cap, dest_buf)
        }
    }
}
