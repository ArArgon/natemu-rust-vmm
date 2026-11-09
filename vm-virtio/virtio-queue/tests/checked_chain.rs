// SPDX-License-Identifier: Apache-2.0 AND BSD-3-Clause

use virtio_bindings::bindings::virtio_ring::{VRING_DESC_F_INDIRECT, VRING_DESC_F_NEXT};
use virtio_queue::{desc::split::Descriptor, CheckedDescriptorChain, Error};
use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap};

const NEXT: u16 = VRING_DESC_F_NEXT as u16;
const INDIRECT: u16 = VRING_DESC_F_INDIRECT as u16;

fn memory() -> GuestMemoryMmap {
    GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x1000)]).unwrap()
}

fn put(mem: &GuestMemoryMmap, at: u64, addr: u64, len: u32, flags: u16, next: u16) {
    mem.write_obj(Descriptor::new(addr, len, flags, next), GuestAddress(at))
        .unwrap();
}

#[test]
fn constructor_checks_configuration_without_reading_table() {
    let mem = memory();
    for size in [0, 3, u16::MAX] {
        assert!(matches!(
            CheckedDescriptorChain::new(&mem, GuestAddress(0), size, 0, false),
            Err(Error::InvalidSize)
        ));
    }
    assert!(matches!(
        CheckedDescriptorChain::new(&mem, GuestAddress(1), 2, 0, false),
        Err(Error::InvalidDescTableAlign)
    ));
    assert!(matches!(
        CheckedDescriptorChain::new(&mem, GuestAddress(0), 2, 2, false),
        Err(Error::InvalidDescriptorIndex)
    ));
    assert!(matches!(
        CheckedDescriptorChain::new(&mem, GuestAddress(u64::MAX - 15), 2, 0, false),
        Err(Error::AddressOverflow)
    ));
    assert!(CheckedDescriptorChain::new(&mem, GuestAddress(0x2000), 2, 0, false).is_ok());
}

#[test]
fn arbitrary_head_and_exact_queue_length_end_cleanly() {
    let mem = memory();
    put(&mem, 16, 0x800, 4, NEXT, 0);
    put(&mem, 0, 0x900, 0, 0, 0);
    let mut chain = CheckedDescriptorChain::new(&mem, GuestAddress(0), 2, 1, false).unwrap();
    assert_eq!(chain.head_index(), 1);
    assert_eq!(chain.next().unwrap().unwrap().addr(), GuestAddress(0x800));
    assert_eq!(chain.next().unwrap().unwrap().len(), 0);
    assert!(chain.next().is_none());
    assert!(chain.next().is_none());
}

#[test]
fn bad_next_is_not_a_clean_end_and_error_is_terminal() {
    let mem = memory();
    put(&mem, 0, 0x800, 1, NEXT, 4);
    let mut chain = CheckedDescriptorChain::new(&mem, GuestAddress(0), 4, 0, false).unwrap();
    assert!(chain.next().unwrap().is_ok());
    assert!(matches!(
        chain.next(),
        Some(Err(Error::InvalidDescriptorIndex))
    ));
    put(&mem, 0, 0x800, 1, 0, 0);
    assert!(chain.next().is_none());
}

#[test]
fn cycles_exhaust_the_budget_with_an_error() {
    let mem = memory();
    put(&mem, 0, 0x800, 1, NEXT, 0);
    let mut chain = CheckedDescriptorChain::new(&mem, GuestAddress(0), 2, 0, false).unwrap();
    assert!(chain.next().unwrap().is_ok());
    assert!(chain.next().unwrap().is_ok());
    assert!(matches!(chain.next(), Some(Err(Error::InvalidChain))));
    assert!(chain.next().is_none());
}

#[test]
fn unmapped_and_truncated_tables_report_memory_errors() {
    let mem = memory();
    let mut absent = CheckedDescriptorChain::new(&mem, GuestAddress(0x2000), 2, 0, false).unwrap();
    assert!(matches!(absent.next(), Some(Err(Error::GuestMemory(_)))));
    assert!(absent.next().is_none());
    put(&mem, 0xff0, 0x800, 1, NEXT, 1);
    let mut truncated =
        CheckedDescriptorChain::new(&mem, GuestAddress(0xff0), 2, 0, false).unwrap();
    assert!(truncated.next().unwrap().is_ok());
    assert!(matches!(truncated.next(), Some(Err(Error::GuestMemory(_)))));
}

#[test]
fn length_overflow_invalidates_the_chain_not_just_its_suffix() {
    let mem = memory();
    put(&mem, 0, 0x800, u32::MAX, NEXT, 1);
    put(&mem, 16, 0x900, 1, 0, 0);
    let chain = CheckedDescriptorChain::new(&mem, GuestAddress(0), 2, 0, false).unwrap();
    assert!(matches!(
        chain.collect::<Result<Vec<_>, _>>(),
        Err(Error::DescriptorChainOverflow)
    ));
}

#[test]
fn indirect_permission_is_checked_before_reading_its_target() {
    let mem = memory();
    put(&mem, 0, 0x2000, 16, INDIRECT, 0);
    let mut forbidden = CheckedDescriptorChain::new(&mem, GuestAddress(0), 2, 0, false).unwrap();
    assert!(matches!(
        forbidden.next(),
        Some(Err(Error::InvalidIndirectDescriptor))
    ));
    let mut allowed = CheckedDescriptorChain::new(&mem, GuestAddress(0), 2, 0, true).unwrap();
    assert!(matches!(allowed.next(), Some(Err(Error::GuestMemory(_)))));
}

#[test]
fn allowed_indirect_chain_can_have_an_unaligned_table() {
    let mem = memory();
    put(&mem, 0, 0x801, 32, INDIRECT, 0);
    put(&mem, 0x801, 0xa00, 4, NEXT, 1);
    put(&mem, 0x811, 0xb00, 8, 0, 0);
    let chain = CheckedDescriptorChain::new(&mem, GuestAddress(0), 2, 0, true).unwrap();
    let descs = chain.collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(descs.len(), 2);
    assert_eq!(descs[1].addr(), GuestAddress(0xb00));
}

#[test]
fn malformed_and_nested_indirect_tables_are_rejected() {
    let mem = memory();
    for len in [0, 17, (u32::from(u16::MAX) + 1) * 16] {
        put(&mem, 0, 0x800, len, INDIRECT, 0);
        let mut chain = CheckedDescriptorChain::new(&mem, GuestAddress(0), 2, 0, true).unwrap();
        assert!(matches!(
            chain.next(),
            Some(Err(Error::InvalidIndirectDescriptorTable))
        ));
    }
    put(&mem, 0, 0x800, 16, INDIRECT | NEXT, 1);
    let mut both = CheckedDescriptorChain::new(&mem, GuestAddress(0), 2, 0, true).unwrap();
    assert!(matches!(
        both.next(),
        Some(Err(Error::InvalidIndirectDescriptor))
    ));
    put(&mem, 0, 0x800, 16, INDIRECT, 0);
    put(&mem, 0x800, 0x900, 16, INDIRECT, 0);
    let mut nested = CheckedDescriptorChain::new(&mem, GuestAddress(0), 2, 0, true).unwrap();
    assert!(matches!(
        nested.next(),
        Some(Err(Error::InvalidIndirectDescriptor))
    ));
}

#[test]
fn indirect_switch_does_not_reset_the_checked_chain_budget() {
    let mem = memory();
    put(&mem, 0, 0xa00, 1, NEXT, 1);
    put(&mem, 16, 0x800, 32, INDIRECT, 0);
    put(&mem, 0x800, 0xb00, 1, NEXT, 1);
    put(&mem, 0x810, 0xc00, 1, 0, 0);
    let chain = CheckedDescriptorChain::new(&mem, GuestAddress(0), 2, 0, true).unwrap();
    assert!(matches!(
        chain.collect::<Result<Vec<_>, _>>(),
        Err(Error::InvalidChain)
    ));
}
