// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.

use std::fmt::Formatter;
use std::{fmt, result};

#[cfg(target_arch = "x86_64")]
use kvm_bindings::{
    kvm_clock_data, kvm_irqchip, kvm_pit_config, kvm_pit_state2, CpuId, MsrList,
    KVM_CLOCK_TSC_STABLE, KVM_IRQCHIP_IOAPIC, KVM_IRQCHIP_PIC_MASTER, KVM_IRQCHIP_PIC_SLAVE,
    KVM_MAX_CPUID_ENTRIES, KVM_PIT_SPEAKER_DUMMY,
};
use kvm_bindings::{kvm_userspace_memory_region, KVM_MEM_LOG_DIRTY_PAGES};
use kvm_ioctls::{Kvm, VmFd};
use versionize::{VersionMap, Versionize, VersionizeResult};
use versionize_derive::Versionize;
use vm_memory::{Address, GuestMemory, GuestMemoryMmap, GuestMemoryRegion};

#[cfg(target_arch = "aarch64")]
use crate::arch::aarch64::gic::GICDevice;
#[cfg(target_arch = "aarch64")]
use crate::arch::aarch64::gic::GicState;

/// Errors associated with the wrappers over KVM ioctls.
#[derive(Debug)]
pub enum Error {
    #[cfg(target_arch = "x86_64")]
    /// Retrieving supported guest MSRs fails.
    GuestMSRs(crate::arch::x86_64::msr::Error),
    /// The number of configured slots is bigger than the maximum reported by KVM.
    NotEnoughMemorySlots,
    /// Cannot set the memory regions.
    SetUserMemoryRegion(kvm_ioctls::Error),
    #[cfg(target_arch = "aarch64")]
    /// Cannot create the global interrupt controller..
    VmCreateGIC(crate::arch::aarch64::gic::Error),
    /// Cannot open the VM file descriptor.
    VmFd(kvm_ioctls::Error),
    #[cfg(target_arch = "x86_64")]
    /// Failed to get KVM vm pit state.
    VmGetPit2(kvm_ioctls::Error),
    #[cfg(target_arch = "x86_64")]
    /// Failed to get KVM vm clock.
    VmGetClock(kvm_ioctls::Error),
    #[cfg(target_arch = "x86_64")]
    /// Failed to get KVM vm irqchip.
    VmGetIrqChip(kvm_ioctls::Error),
    #[cfg(target_arch = "x86_64")]
    /// Failed to set KVM vm pit state.
    VmSetPit2(kvm_ioctls::Error),
    #[cfg(target_arch = "x86_64")]
    /// Failed to set KVM vm clock.
    VmSetClock(kvm_ioctls::Error),
    #[cfg(target_arch = "x86_64")]
    /// Failed to set KVM vm irqchip.
    VmSetIrqChip(kvm_ioctls::Error),
    /// Cannot configure the microvm.
    VmSetup(kvm_ioctls::Error),
    #[cfg(target_arch = "aarch64")]
    SaveGic(crate::arch::aarch64::gic::Error),
    #[cfg(target_arch = "aarch64")]
    RestoreGic(crate::arch::aarch64::gic::Error),
}

/// Error type for [`Vm::restore_state`]
#[cfg(target_arch = "x86_64")]
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RestoreStateError {
    #[error("{0}")]
    SetPit2(kvm_ioctls::Error),
    #[error("{0}")]
    SetClock(kvm_ioctls::Error),
    #[error("{0}")]
    SetIrqChipPicMaster(kvm_ioctls::Error),
    #[error("{0}")]
    SetIrqChipPicSlave(kvm_ioctls::Error),
    #[error("{0}")]
    SetIrqChipIoAPIC(kvm_ioctls::Error),
}

/// Error type for [`Vm::restore_state`]
#[cfg(target_arch = "aarch64")]
#[derive(Debug, derive_more::From)]
pub struct RestoreStateError(crate::arch::aarch64::gic::Error);
#[cfg(target_arch = "aarch64")]
impl fmt::Display for RestoreStateError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
#[cfg(target_arch = "aarch64")]
impl std::error::Error for RestoreStateError {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        use self::Error::*;

        match self {
            #[cfg(target_arch = "x86_64")]
            GuestMSRs(err) => write!(f, "Retrieving supported guest MSRs fails: {:?}", err),
            #[cfg(target_arch = "aarch64")]
            VmCreateGIC(err) => write!(
                f,
                "Error creating the global interrupt controller: {:?}",
                err
            ),
            VmFd(err) => write!(f, "Cannot open the VM file descriptor: {}", err),
            VmSetup(err) => write!(f, "Cannot configure the microvm: {}", err),
            NotEnoughMemorySlots => write!(
                f,
                "The number of configured slots is bigger than the maximum reported by KVM"
            ),
            SetUserMemoryRegion(err) => write!(f, "Cannot set the memory regions: {}", err),
            #[cfg(target_arch = "x86_64")]
            VmGetPit2(err) => write!(f, "Failed to get KVM vm pit state: {}", err),
            #[cfg(target_arch = "x86_64")]
            VmGetClock(err) => write!(f, "Failed to get KVM vm clock: {}", err),
            #[cfg(target_arch = "x86_64")]
            VmGetIrqChip(err) => write!(f, "Failed to get KVM vm irqchip: {}", err),
            #[cfg(target_arch = "x86_64")]
            VmSetPit2(err) => write!(f, "Failed to set KVM vm pit state: {}", err),
            #[cfg(target_arch = "x86_64")]
            VmSetClock(err) => write!(f, "Failed to set KVM vm clock: {}", err),
            #[cfg(target_arch = "x86_64")]
            VmSetIrqChip(err) => write!(f, "Failed to set KVM vm irqchip: {}", err),
            #[cfg(target_arch = "aarch64")]
            SaveGic(err) => write!(f, "Failed to save the VM's GIC state: {:?}", err),
            #[cfg(target_arch = "aarch64")]
            RestoreGic(err) => write!(f, "Failed to restore the VM's GIC state: {:?}", err),
        }
    }
}

pub type Result<T> = result::Result<T, Error>;

/// A wrapper around creating and using a VM.
pub struct Vm {
    fd: VmFd,

    // X86 specific fields.
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    supported_cpuid: CpuId,
    #[cfg(target_arch = "x86_64")]
    supported_msrs: MsrList,

    // Arm specific fields.
    // On aarch64 we need to keep around the fd obtained by creating the VGIC device.
    #[cfg(target_arch = "aarch64")]
    irqchip_handle: Option<Box<dyn GICDevice>>,
}

impl Vm {
    /// Constructs a new `Vm` using the given `Kvm` instance.
    pub fn new(kvm: &Kvm) -> Result<Self> {
        // Create fd for interacting with kvm-vm specific functions.
        let vm_fd = kvm.create_vm().map_err(Error::VmFd)?;

        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        let supported_cpuid = kvm
            .get_supported_cpuid(KVM_MAX_CPUID_ENTRIES)
            .map_err(Error::VmFd)?;
        #[cfg(target_arch = "x86_64")]
        let supported_msrs =
            crate::arch::x86_64::msr::supported_guest_msrs(kvm).map_err(Error::GuestMSRs)?;

        Ok(Vm {
            fd: vm_fd,
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            supported_cpuid,
            #[cfg(target_arch = "x86_64")]
            supported_msrs,
            #[cfg(target_arch = "aarch64")]
            irqchip_handle: None,
        })
    }

    /// Returns a ref to the supported `CpuId` for this Vm.
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    pub fn supported_cpuid(&self) -> &CpuId {
        &self.supported_cpuid
    }

    /// Returns a ref to the supported `MsrList` for this Vm.
    #[cfg(target_arch = "x86_64")]
    pub fn supported_msrs(&self) -> &MsrList {
        &self.supported_msrs
    }

    /// Initializes the guest memory.
    pub fn memory_init(
        &mut self,
        guest_mem: &GuestMemoryMmap,
        kvm_max_memslots: usize,
        track_dirty_pages: bool,
    ) -> Result<()> {
        if guest_mem.num_regions() > kvm_max_memslots {
            return Err(Error::NotEnoughMemorySlots);
        }
        self.set_kvm_memory_regions(guest_mem, track_dirty_pages)?;
        #[cfg(target_arch = "x86_64")]
        self.fd
            .set_tss_address(crate::arch::x86_64::layout::KVM_TSS_ADDRESS as usize)
            .map_err(Error::VmSetup)?;

        Ok(())
    }

    /// Creates the irq chip and an in-kernel device model for the PIT.
    #[cfg(target_arch = "x86_64")]
    pub fn setup_irqchip(&self) -> Result<()> {
        self.fd.create_irq_chip().map_err(Error::VmSetup)?;
        // We need to enable the emulation of a dummy speaker port stub so that writing to port 0x61
        // (i.e. KVM_SPEAKER_BASE_ADDRESS) does not trigger an exit to user space.
        let pit_config = kvm_pit_config {
            flags: KVM_PIT_SPEAKER_DUMMY,
            ..Default::default()
        };
        self.fd.create_pit2(pit_config).map_err(Error::VmSetup)
    }

    /// Creates the GIC (Global Interrupt Controller).
    #[cfg(target_arch = "aarch64")]
    pub fn setup_irqchip(&mut self, vcpu_count: u8) -> Result<()> {
        self.irqchip_handle = Some(
            crate::arch::aarch64::gic::create_gic(&self.fd, vcpu_count.into(), None)
                .map_err(Error::VmCreateGIC)?,
        );
        Ok(())
    }

    /// Gets a reference to the irqchip of the VM.
    #[cfg(target_arch = "aarch64")]
    pub fn get_irqchip(&self) -> &dyn GICDevice {
        self.irqchip_handle
            .as_ref()
            .expect("IRQ chip not set")
            .as_ref()
    }

    /// Gets a reference to the kvm file descriptor owned by this VM.
    pub fn fd(&self) -> &VmFd {
        &self.fd
    }

    #[cfg(target_arch = "x86_64")]
    /// Saves and returns the Kvm Vm state.
    pub fn save_state(&self) -> Result<VmState> {
        let pitstate = self.fd.get_pit2().map_err(Error::VmGetPit2)?;

        let mut clock = self.fd.get_clock().map_err(Error::VmGetClock)?;
        // This bit is not accepted in SET_CLOCK, clear it.
        clock.flags &= !KVM_CLOCK_TSC_STABLE;

        let mut pic_master = kvm_irqchip {
            chip_id: KVM_IRQCHIP_PIC_MASTER,
            ..Default::default()
        };
        self.fd
            .get_irqchip(&mut pic_master)
            .map_err(Error::VmGetIrqChip)?;

        let mut pic_slave = kvm_irqchip {
            chip_id: KVM_IRQCHIP_PIC_SLAVE,
            ..Default::default()
        };
        self.fd
            .get_irqchip(&mut pic_slave)
            .map_err(Error::VmGetIrqChip)?;

        let mut ioapic = kvm_irqchip {
            chip_id: KVM_IRQCHIP_IOAPIC,
            ..Default::default()
        };
        self.fd
            .get_irqchip(&mut ioapic)
            .map_err(Error::VmGetIrqChip)?;

        Ok(VmState {
            pitstate,
            clock,
            pic_master,
            pic_slave,
            ioapic,
        })
    }

    #[cfg(target_arch = "x86_64")]
    /// Restores the KVM VM state.
    ///
    /// # Errors
    ///
    /// When:
    /// - [`kvm_ioctls::VmFd::set_pit`] errors.
    /// - [`kvm_ioctls::VmFd::set_clock`] errors.
    /// - [`kvm_ioctls::VmFd::set_irqchip`] errors.
    /// - [`kvm_ioctls::VmFd::set_irqchip`] errors.
    /// - [`kvm_ioctls::VmFd::set_irqchip`] errors.
    pub fn restore_state(&self, state: &VmState) -> std::result::Result<(), RestoreStateError> {
        self.fd
            .set_pit2(&state.pitstate)
            .map_err(RestoreStateError::SetPit2)?;
        self.fd
            .set_clock(&state.clock)
            .map_err(RestoreStateError::SetClock)?;
        self.fd
            .set_irqchip(&state.pic_master)
            .map_err(RestoreStateError::SetIrqChipPicMaster)?;
        self.fd
            .set_irqchip(&state.pic_slave)
            .map_err(RestoreStateError::SetIrqChipPicSlave)?;
        self.fd
            .set_irqchip(&state.ioapic)
            .map_err(RestoreStateError::SetIrqChipIoAPIC)?;
        Ok(())
    }

    #[cfg(target_arch = "aarch64")]
    /// Saves and returns the Kvm Vm state.
    pub fn save_state(&self, mpidrs: &[u64]) -> Result<VmState> {
        Ok(VmState {
            gic: self
                .get_irqchip()
                .save_device(mpidrs)
                .map_err(Error::SaveGic)?,
        })
    }

    /// Restore the KVM VM state
    ///
    /// # Errors
    ///
    /// When [`GICDevice::restore_device`] errors.
    #[cfg(target_arch = "aarch64")]
    pub fn restore_state(
        &self,
        mpidrs: &[u64],
        state: &VmState,
    ) -> std::result::Result<(), RestoreStateError> {
        self.get_irqchip()
            .restore_device(mpidrs, &state.gic)
            .map_err(RestoreStateError)
    }

    pub(crate) fn set_kvm_memory_regions(
        &self,
        guest_mem: &GuestMemoryMmap,
        track_dirty_pages: bool,
    ) -> Result<()> {
        let mut flags = 0u32;
        if track_dirty_pages {
            flags |= KVM_MEM_LOG_DIRTY_PAGES;
        }
        guest_mem
            .iter()
            .enumerate()
            .try_for_each(|(index, region)| {
                let memory_region = kvm_userspace_memory_region {
                    slot: index as u32,
                    guest_phys_addr: region.start_addr().raw_value(),
                    memory_size: region.len(),
                    // It's safe to unwrap because the guest address is valid.
                    userspace_addr: guest_mem.get_host_address(region.start_addr()).unwrap() as u64,
                    flags,
                };

                // SAFETY: Safe because the fd is a valid KVM file descriptor.
                unsafe { self.fd.set_user_memory_region(memory_region) }
            })
            .map_err(Error::SetUserMemoryRegion)?;
        Ok(())
    }
}

#[cfg(target_arch = "x86_64")]
#[derive(Versionize)]
/// Structure holding VM kvm state.
// NOTICE: Any changes to this structure require a snapshot version bump.
pub struct VmState {
    pitstate: kvm_pit_state2,
    clock: kvm_clock_data,
    // TODO: rename this field to adopt inclusive language once Linux updates it, too.
    pic_master: kvm_irqchip,
    // TODO: rename this field to adopt inclusive language once Linux updates it, too.
    pic_slave: kvm_irqchip,
    ioapic: kvm_irqchip,
}

/// Structure holding an general specific VM state.
#[cfg(target_arch = "aarch64")]
#[derive(Default, Versionize)]
pub struct VmState {
    gic: GicState,
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::undocumented_unsafe_blocks)]
    use std::os::unix::io::FromRawFd;

    use vm_memory::GuestAddress;

    use super::*;
    use crate::vstate::system::KvmContext;

    // Auxiliary function being used throughout the tests.
    pub(crate) fn setup_vm(mem_size: usize) -> (Vm, GuestMemoryMmap) {
        let kvm = KvmContext::new().unwrap();
        let gm =
            vm_memory::test_utils::create_anon_guest_memory(&[(GuestAddress(0), mem_size)], false)
                .unwrap();

        let mut vm = Vm::new(kvm.fd()).expect("Cannot create new vm");
        assert!(vm.memory_init(&gm, kvm.max_memslots(), false).is_ok());

        (vm, gm)
    }

    #[test]
    fn test_new() {
        use std::os::unix::io::AsRawFd;

        use utils::tempfile::TempFile;
        // Testing an error case.
        let vm =
            Vm::new(&unsafe { Kvm::from_raw_fd(TempFile::new().unwrap().as_file().as_raw_fd()) });
        assert!(vm.is_err());

        // Testing with a valid /dev/kvm descriptor.
        let kvm = KvmContext::new().unwrap();
        assert!(Vm::new(kvm.fd()).is_ok());
    }

    #[test]
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    fn test_get_supported_cpuid() {
        let kvm = KvmContext::new().unwrap();
        let vm = Vm::new(kvm.fd()).expect("Cannot create new vm");
        let cpuid = kvm
            .fd()
            .get_supported_cpuid(KVM_MAX_CPUID_ENTRIES)
            .expect("Cannot get supported cpuid");
        assert_eq!(vm.supported_cpuid().as_slice(), cpuid.as_slice());
    }

    #[test]
    fn test_vm_memory_init() {
        let kvm_context = KvmContext::new().unwrap();
        let mut vm = Vm::new(kvm_context.fd()).expect("Cannot create new vm");

        // Create valid memory region and test that the initialization is successful.
        let gm =
            vm_memory::test_utils::create_anon_guest_memory(&[(GuestAddress(0), 0x1000)], false)
                .unwrap();
        assert!(vm
            .memory_init(&gm, kvm_context.max_memslots(), true)
            .is_ok());
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_vm_save_restore_state() {
        let kvm_fd = Kvm::new().unwrap();
        let vm = Vm::new(&kvm_fd).expect("new vm failed");
        // Irqchips, clock and pitstate are not configured so trying to save state should fail.
        assert!(vm.save_state().is_err());

        let (vm, _mem) = setup_vm(0x1000);
        vm.setup_irqchip().unwrap();

        let vm_state = vm.save_state().unwrap();
        assert_eq!(
            vm_state.pitstate.flags | KVM_PIT_SPEAKER_DUMMY,
            KVM_PIT_SPEAKER_DUMMY
        );
        assert_eq!(vm_state.clock.flags & KVM_CLOCK_TSC_STABLE, 0);
        assert_eq!(vm_state.pic_master.chip_id, KVM_IRQCHIP_PIC_MASTER);
        assert_eq!(vm_state.pic_slave.chip_id, KVM_IRQCHIP_PIC_SLAVE);
        assert_eq!(vm_state.ioapic.chip_id, KVM_IRQCHIP_IOAPIC);

        let (vm, _mem) = setup_vm(0x1000);
        vm.setup_irqchip().unwrap();

        assert!(vm.restore_state(&vm_state).is_ok());
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_vm_save_restore_state_bad_irqchip() {
        use kvm_bindings::KVM_NR_IRQCHIPS;

        let (vm, _mem) = setup_vm(0x1000);
        vm.setup_irqchip().unwrap();
        let mut vm_state = vm.save_state().unwrap();

        let (vm, _mem) = setup_vm(0x1000);
        vm.setup_irqchip().unwrap();

        // Try to restore an invalid PIC Master chip ID
        let orig_master_chip_id = vm_state.pic_master.chip_id;
        vm_state.pic_master.chip_id = KVM_NR_IRQCHIPS;
        assert!(vm.restore_state(&vm_state).is_err());
        vm_state.pic_master.chip_id = orig_master_chip_id;

        // Try to restore an invalid PIC Slave chip ID
        let orig_slave_chip_id = vm_state.pic_slave.chip_id;
        vm_state.pic_slave.chip_id = KVM_NR_IRQCHIPS;
        assert!(vm.restore_state(&vm_state).is_err());
        vm_state.pic_slave.chip_id = orig_slave_chip_id;

        // Try to restore an invalid IOPIC chip ID
        vm_state.ioapic.chip_id = KVM_NR_IRQCHIPS;
        assert!(vm.restore_state(&vm_state).is_err());
    }

    #[test]
    fn test_set_kvm_memory_regions() {
        let kvm_context = KvmContext::new().unwrap();
        let vm = Vm::new(kvm_context.fd()).expect("Cannot create new vm");

        let gm =
            vm_memory::test_utils::create_anon_guest_memory(&[(GuestAddress(0), 0x1000)], false)
                .unwrap();
        let res = vm.set_kvm_memory_regions(&gm, false);
        assert!(res.is_ok());

        // Trying to set a memory region with a size that is not a multiple of PAGE_SIZE
        // will result in error.
        let gm =
            vm_memory::test_utils::create_guest_memory_unguarded(&[(GuestAddress(0), 0x10)], false)
                .unwrap();
        let res = vm.set_kvm_memory_regions(&gm, false);
        assert_eq!(
            res.unwrap_err().to_string(),
            "Cannot set the memory regions: Invalid argument (os error 22)"
        );
    }
}
