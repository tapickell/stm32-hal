//! Read and write onboard flash memory.

// Note that most of the code for L5 and U5 is feature-gated due to different
// register names, differentiating secure and non-secure. We keep them in the same file
// as the other families, to demonstrate that the code is still mostly the same; this
// makes it easier to maintain than separate files.

// todo: Between L5 and the rest, and L5 secure and non, there's a lot of DRY!
// todo: The answer may be macros.

// todo: Review the instructions regarding error checking, eg the first few steps in write.
// todo: Is our code (unlock, check_illegal especially) doing that?

// Based on `stm32l4xx-hal`.

use crate::pac::FLASH;
use core;

const FLASH_KEY1: u32 = 0x4567_0123;
const FLASH_KEY2: u32 = 0xCDEF_89AB;

#[cfg(feature = "l5")]
#[derive(Clone, Copy)]
/// Cortex-M33 secure programming, or nonsecure.
pub enum Security {
    NonSecure,
    Secure,
}

#[derive(Clone, Copy)]
/// Set dual bank mode (DBANK option bit)
enum _DualBank {
    Dual,
    Single,
}

#[derive(Clone, Copy)]
pub enum BanksToErase {
    Bank1,
    Bank2,
    Both,
}

#[derive(Copy, Clone, Debug)]
pub enum Error {
    /// Flash controller is not done yet
    Busy,
    /// Error detected (by command execution, or because no command could be executed)
    Illegal,
    /// Set during read if ECC decoding logic detects correctable or uncorrectable error
    EccError,
    /// Page number is out of range
    PageOutOfRange,
    /// (Legal) command failed
    Failure,
}

#[cfg(not(feature = "l5"))]
/// Check and clear all non-secure error programming flags due to a previous
/// programming. If not, PGSERR is set.
fn check_illegal(flash: &FLASH) -> Result<(), Error> {
    // todo: QC this fn and its l5 variant.
    let sr = flash.sr.read();
    cfg_if::cfg_if! {
        if #[cfg(any(feature = "f3"))] {
            if sr.pgerr().bit_is_set() || sr.wrprterr().bit_is_set() {
                return Err(Error::Illegal);
            }
        } else if #[cfg(any(feature = "f4"))] {
            if sr.pgaerr().bit_is_set() {  // todo: Others for f4?
                return Err(Error::Illegal);
            }
        } else {
            if sr.pgaerr().bit_is_set() || sr.progerr().bit_is_set() || sr.wrperr().bit_is_set() {
                return Err(Error::Illegal);
            }
        }
    }
    Ok(())
}

#[cfg(feature = "l5")]
/// Check and clear all non-secure error programming flags due to a previous
/// programming. If not, NSPGSERR is set.
fn check_illegal(flash: &FLASH, security: Security) -> Result<(), Error> {
    match security {
        Security::NonSecure => {
            let sr = flash.nssr.read();
            if sr.nspgaerr().bit_is_set()
                || sr.nspgserr().bit_is_set()
                || sr.nsprogerr().bit_is_set()
                || sr.nswrperr().bit_is_set()
            {
                return Err(Error::Illegal);
            }
        }
        Security::Secure => {
            let sr = flash.secsr.read();
            if sr.secpgaerr().bit_is_set()
                || sr.secpgaerr().bit_is_set()
                || sr.secprogerr().bit_is_set()
                || sr.secwrperr().bit_is_set()
            {
                return Err(Error::Illegal);
            }
        }
    }

    Ok(())
}

pub struct Flash {
    pub(crate) regs: FLASH,
}

/// The Flash memory is organized as 72-bit wide memory cells (64 bits plus 8 ECC bits) that
/// can be used for storing both code and data constants.
impl Flash {
    pub fn new(regs: FLASH) -> Self {
        // todo: Implement and configure dual bank mode.
        Self { regs }
    }

    #[cfg(feature = "l5")]
    /// Unlock the flash memory, allowing writes. See L4 Reference manual, section 6.3.5.
    pub fn unlock(&mut self, security: Security) -> Result<(), Error> {
        match security {
            Security::NonSecure => {
                self.regs.nskeyr.write(|w| unsafe { w.bits(FLASH_KEY1) });
                self.regs.nskeyr.write(|w| unsafe { w.bits(FLASH_KEY2) });

                if self.regs.nscr.read().nslock().bit_is_clear() {
                    Ok(())
                } else {
                    Err(Error::Failure)
                }
            }
            Security::Secure => {
                self.regs.seckeyr.write(|w| unsafe { w.bits(FLASH_KEY1) });
                self.regs.seckeyr.write(|w| unsafe { w.bits(FLASH_KEY2) });

                if self.regs.seccr.read().seclock().bit_is_clear() {
                    Ok(())
                } else {
                    Err(Error::Failure)
                }
            }
        }
    }

    #[cfg(not(feature = "l5"))]
    /// Unlock the flash memory, allowing writes. See L4 Reference manual, section 3.3.5.
    pub fn unlock(&mut self) -> Result<(), Error> {
        self.regs.keyr.write(|w| unsafe { w.bits(FLASH_KEY1) });
        self.regs.keyr.write(|w| unsafe { w.bits(FLASH_KEY2) });

        if self.regs.cr.read().lock().bit_is_clear() {
            Ok(())
        } else {
            Err(Error::Failure)
        }
    }

    #[cfg(not(feature = "l5"))]
    pub fn lock(&mut self) {
        self.regs.cr.modify(|_, w| w.lock().set_bit());
    }

    #[cfg(feature = "l5")]
    /// Lock the flash memory, allowing writes.
    pub fn lock(&mut self, security: Security) {
        match security {
            Security::NonSecure => self.regs.nscr.modify(|_, w| w.nslock().set_bit()),
            Security::Secure => self.regs.seccr.modify(|_, w| w.seclock().set_bit()),
        };
    }

    #[cfg(not(feature = "l5"))]
    /// Erase an entire page. See L4 Reference manual, section 3.3.5.
    /// For why this is required, reference L4 RM, section 3.3.7:
    /// "Programming in a previously programmed address is not allowed except if the data to write
    /// is full zero, and any attempt will set PROGERR flag in the Flash status register
    /// (FLASH_SR)."
    pub fn erase_page(&mut self, page: usize) -> Result<(), Error> {
        self.unlock()?;

        // 1. Check that no Flash memory operation is ongoing by checking the BSY bit in the Flash
        // status register (FLASH_SR).
        let sr = self.regs.sr.read();
        if sr.bsy().bit_is_set() {
            self.lock();
            return Err(Error::Busy);
        }

        // 2. Check and clear all error programming flags due to a previous programming. If not,
        // PGSERR is set.
        if check_illegal(&self.regs).is_err() {
            self.lock();
            return Err(Error::Illegal);
        };

        // 3. Set the PER bit and select the page you wish to erase (PNB) with the associated bank
        // (BKER) in the Flash control register (FLASH_CR).

        // Note that `STM32L4` includes the `.bker()` bit to select banks for all variants, but
        // some variants only have 1 memory bank; eg ones with a smaller amount of memory.

        cfg_if::cfg_if! {
            if #[cfg(feature = "f3")] {
                // F3 RM: "Erase procedure"
                // Set the PER bit in the FLASH_CR register
                self.regs.cr.modify(|_, w| w.per().set_bit());

                // Program the FLASH_CR register
                // self.regs.ar.modify(|_, w| w.far().bits(page as u8));
                self.regs.ar.write(|w| unsafe { w.bits(page as u32) }); // todo: Is this right?
            } else if #[cfg(feature = "f4")] {
                // Set the SER bit and select the sector out of the 12 sectors (for STM32F405xx/07xx and
                // STM32F415xx/17xx) and out of 24 (for STM32F42xxx and STM32F43xxx) in the main
                // memory block you wish to erase (SNB) in the FLASH_CR register
                self.regs.cr.modify(|_, w| unsafe {
                    w.ser().set_bit();
                    w.snb().bits(page as u8) // todo: Probably not right?
                });

            } else if #[cfg(any(feature = "g0", feature = "g4"))] {
                 self.regs.cr.modify(|_, w| unsafe {
                    w.pnb()
                    .bits(page as u8)
                    .per()
                    .set_bit()
                });
            } else {
                match page {
                    0..=255 => {
                        self.regs.cr.modify(|_, w| unsafe {
                            w.bker().clear_bit().pnb().bits(page as u8).per().set_bit()
                        });
                    }
                    256..=511 => {
                        self.regs.cr.modify(|_, w| unsafe {
                            w.bker()
                                .set_bit()
                                .pnb()
                                .bits((page - 256) as u8)
                                .per()
                                .set_bit()
                        });
                    }
                    _ => {
                        return Err(Error::PageOutOfRange);
                    }
                }
            }
        }

        // 4. Set the STRT bit in the FLASH_CR register.
        cfg_if::cfg_if! {
            if #[cfg(any(feature = "f3", feature = "f4"))] {
                self.regs.cr.modify(|_, w| w.strt().set_bit());
            } else {
                #[cfg(any(feature = "g0", feature = "g4"))]
                self.regs.cr.modify(|_, w| w.strt().set_bit());
                #[cfg(not(any(feature = "g0", feature = "g4")))]
                self.regs.cr.modify(|_, w| w.start().set_bit());
            }
        }

        // 5. Wait for the BSY bit to be cleared in the FLASH_SR register.
        while self.regs.sr.read().bsy().bit_is_set() {}

        // todo on F3: "Read the erased option bytes and verify" as final step
        cfg_if::cfg_if! {
            if #[cfg(any(feature = "f3", feature = "f4"))] {
                // Check the EOP flag in the FLASH_SR register (it is set when the erase operation has
                // succeeded), and then clear it by software. (todo)

                // Clear the EOP flag
                self.regs.sr.modify(|_, w| w.eop().set_bit());
            } else {
                self.regs.cr.modify(|_, w| w.per().clear_bit());
            }
        }

        self.lock();

        Ok(())
    }

    #[cfg(feature = "l5")]
    /// Erase an entire page. See L5 Reference manual, section 6.3.6.
    /// For why this is required, reference L4 RM, section 3.3.7:
    /// "Programming in a previously programmed address is not allowed except if the data to write
    /// is full zero, and any attempt will set PROGERR flag in the Flash status register
    /// (FLASH_SR)."
    pub fn erase_page(&mut self, page: usize, security: Security) -> Result<(), Error> {
        self.unlock(security)?;

        match security {
            Security::NonSecure => {
                // 1. Check that no Flash memory operation is ongoing by checking the NSBSY bit in the Flash
                // status register (FLASH_NSSR).
                let sr = self.regs.nssr.read();
                if sr.nsbsy().bit_is_set() {
                    self.lock(security);
                    return Err(Error::Busy);
                }

                // 2. Check and clear all error programming flags due to a previous programming. If not,
                // NSPGSERR is set.
                if check_illegal(&self.regs, security).is_err() {
                    self.lock(security);
                    return Err(Error::Illegal);
                };

                // 3. In dual-bank mode (DBANK option bit is set), set the NSPER bit and select the
                // non-secure page to erase (NSPNB) with the associated bank (NSBKER) in the
                // FLASH_NSCR. In single-bank mode (DBANK option bit is reset), set the NSPER bit
                // and select the page to erase (NSPNB). The NSBKER bit in the FLASH_NSCR must be
                // kept cleared.
                // todo: Follow that procedure; this may not be right.

                match page {
                    0..=255 => {
                        self.regs.nscr.modify(|_, w| unsafe {
                            w.nsbker()
                                .clear_bit()
                                .nspnb()
                                .bits(page as u8)
                                .nsper()
                                .set_bit()
                        });
                    }
                    256..=511 => {
                        self.regs.nscr.modify(|_, w| unsafe {
                            w.nsbker()
                                .set_bit()
                                .nspnb()
                                .bits((page - 256) as u8)
                                .nsper()
                                .set_bit()
                        });
                    }
                    _ => {
                        return Err(Error::PageOutOfRange);
                    }
                }

                // 4. Set the NSSTRT bit in the FLASH_NSCR register.
                self.regs.nscr.modify(|_, w| w.nsstrt().set_bit());

                // 5. Wait for the NSBSY bit to be cleared in the FLASH_SR register.
                while self.regs.nssr.read().nsbsy().bit_is_set() {}
            }
            Security::Secure => {
                let sr = self.regs.secsr.read();
                if sr.secbsy().bit_is_set() {
                    self.lock(security);
                    return Err(Error::Busy);
                }

                if check_illegal(&self.regs, security).is_err() {
                    self.lock(security);
                    return Err(Error::Illegal);
                };

                match page {
                    0..=255 => {
                        self.regs.seccr.modify(|_, w| unsafe {
                            w.secbker()
                                .clear_bit()
                                .secpnb()
                                .bits(page as u8)
                                .secper()
                                .set_bit()
                        });
                    }
                    256..=511 => {
                        self.regs.seccr.modify(|_, w| unsafe {
                            w.secbker()
                                .set_bit()
                                .secpnb()
                                .bits((page - 256) as u8)
                                .secper()
                                .set_bit()
                        });
                    }
                    _ => {
                        return Err(Error::PageOutOfRange);
                    }
                }

                self.regs.seccr.modify(|_, w| w.secstrt().set_bit());

                while self.regs.secsr.read().secbsy().bit_is_set() {}
            }
        }

        self.lock(security);

        Ok(())
    }

    #[cfg(not(feature = "l5"))]
    pub fn erase_bank(&mut self, banks: BanksToErase) -> Result<(), Error> {
        // todo: DRY
        self.unlock()?;

        // To perform a bank Mass Erase, follow the procedure below:
        // RM0351 Rev 7 105/1903
        // RM0351 Embedded Flash memory (FLASH)
        // 139
        // 1. Check that no Flash memory operation is ongoing by checking the BSY bit in the
        // FLASH_SR register.
        let sr = self.regs.sr.read();
        if sr.bsy().bit_is_set() {
            self.lock();
            return Err(Error::Busy);
        }

        // 2. Check and clear all error programming flags due to a previous programming. If not,
        // PGSERR is set.
        if check_illegal(&self.regs).is_err() {
            self.lock();
            return Err(Error::Illegal);
        };

        // 3. Set the MER1 bit or/and MER2 (depending on the bank) in the Flash control register
        // (FLASH_CR). Both banks can be selected in the same operation.

        cfg_if::cfg_if! {
            if #[cfg(any(feature = "f3", feature = "f4"))] {
                self.regs.cr.modify(|_, w| w.mer().clear_bit());

                // 4. Set the STRT bit in the FLASH_CR register.
                self.regs.cr.modify(|_, w| w.strt().set_bit());
            } else if #[cfg(any(feature = "g0"))] {
                self.regs.cr.modify(|_, w| w.mer().clear_bit());
            } else if #[cfg(any(feature = "g4"))] {
                self.regs.cr.modify(|_, w| w.mer1().clear_bit());
            } else {
                match banks {
                    BanksToErase::Bank1 => {
                        self.regs.cr.modify(|_, w| w.mer1().clear_bit());
                    }
                    BanksToErase::Bank2 => {
                        self.regs.cr.modify(|_, w| w.mer2().clear_bit());
                    }
                    BanksToErase::Both => {
                        self.regs.cr.modify(|_, w| w.mer1().clear_bit());
                        self.regs.cr.modify(|_, w| w.mer2().clear_bit());
                    }
                }

                // 4. Set the STRT bit in the FLASH_CR register.
                #[cfg(feature = "g4")]
                self.regs.cr.modify(|_, w| w.strt().set_bit());
                #[cfg(not(feature = "g4"))]
                self.regs.cr.modify(|_, w| w.start().set_bit());
            }
        }

        // 5. Wait for the BSY bit to be cleared in the FLASH_SR register.
        while self.regs.sr.read().bsy().bit_is_set() {}

        self.lock();

        Ok(())
    }

    #[cfg(feature = "l5")]
    /// Mass erase: L5 RM section 6.3.6
    pub fn erase_bank(&mut self, banks: BanksToErase, security: Security) -> Result<(), Error> {
        self.unlock(security)?;

        match security {
            Security::NonSecure => {
                // To perform a bank Mass Erase, follow the procedure below:

                // 1. Check that no Flash memory operation is ongoing by checking the NSBSY bit in the
                // FLASH_NSSR register.
                let sr = self.regs.nssr.read();
                if sr.nsbsy().bit_is_set() {
                    self.lock(security);
                    return Err(Error::Busy);
                }

                // 2. Check and clear all error programming flags due to a previous programming. If not,
                // NSPGSERR is set.
                if check_illegal(&self.regs, security).is_err() {
                    self.lock(security);
                    return Err(Error::Illegal);
                };

                // 3. Set the MER1 bit or/and MER2 (depending on the bank) in the Flash control register
                // (FLASH_CR). Both banks can be selected in the same operation.
                match banks {
                    BanksToErase::Bank1 => {
                        self.regs.nscr.modify(|_, w| w.nsmer1().clear_bit());
                    }
                    BanksToErase::Bank2 => {
                        self.regs.nscr.modify(|_, w| w.nsmer2().clear_bit());
                    }
                    BanksToErase::Both => {
                        self.regs.nscr.modify(|_, w| w.nsmer1().clear_bit());
                        self.regs.nscr.modify(|_, w| w.nsmer2().clear_bit());
                    }
                }

                // 4. Set the NSSTRT bit in the FLASH_NSCR register.
                self.regs.nscr.modify(|_, w| w.nsstrt().set_bit());

                // 5. Wait for the NSBSY bit to be cleared in the FLASH_NSSR register.
                while self.regs.nssr.read().nsbsy().bit_is_set() {}
            }
            Security::Secure => {
                let sr = self.regs.secsr.read();
                if sr.secbsy().bit_is_set() {
                    self.lock(security);
                    return Err(Error::Busy);
                }

                if check_illegal(&self.regs, security).is_err() {
                    self.lock(security);
                    return Err(Error::Illegal);
                };

                match banks {
                    BanksToErase::Bank1 => {
                        self.regs.seccr.modify(|_, w| w.secmer1().clear_bit());
                    }
                    BanksToErase::Bank2 => {
                        self.regs.seccr.modify(|_, w| w.secmer2().clear_bit());
                    }
                    BanksToErase::Both => {
                        self.regs.seccr.modify(|_, w| w.secmer1().clear_bit());
                        self.regs.seccr.modify(|_, w| w.secmer2().clear_bit());
                    }
                }

                self.regs.seccr.modify(|_, w| w.secstrt().set_bit());

                while self.regs.secsr.read().secbsy().bit_is_set() {}
            }
        }

        self.lock(security);

        Ok(())
    }

    #[cfg(not(feature = "l5"))]
    /// Write the contents of a page. Must be erased first. See L4 RM, section 3.3.7.
    pub fn write_page(&mut self, page: usize, data: &[u64]) -> Result<(), Error> {
        // todo: Consider a u8-based approach.
        // todo: DRY from `erase_page`.
        // The Flash memory programming sequence in standard mode is as follows:
        // 1. Check that no Flash main memory operation is ongoing by checking the BSY bit in the
        // Flash status register (FLASH_SR).
        self.unlock()?;

        let sr = self.regs.sr.read();
        if sr.bsy().bit_is_set() {
            self.lock();
            return Err(Error::Busy);
        }

        // 2. Check and clear all error programming flags due to a previous programming. If not,
        // PGSERR is set.
        if check_illegal(&self.regs).is_err() {
            self.lock();
            return Err(Error::Illegal);
        };

        // 3. Set the PG bit in the Flash control register (FLASH_CR).
        self.regs.cr.modify(|_, w| w.pg().set_bit());

        // 4. Perform the data write operation at the desired memory address, inside main memory
        // block or OTP area. Only double word can be programmed.
        let mut address = page_to_address(page) as *mut u32;

        for dword in data {
            unsafe {
                // – Write a first word in an address aligned with double word
                core::ptr::write_volatile(address, *dword as u32);
                // – Write the second word
                core::ptr::write_volatile(address.add(1), (*dword >> 32) as u32);

                address = address.add(2);
            }

            // 5. Wait until the BSY bit is cleared in the FLASH_SR register.
            while self.regs.sr.read().bsy().bit_is_set() {}

            // 6. Check that EOP flag is set in the FLASH_SR register (meaning that the programming
            // operation has succeed), and clear it by software.
            if self.regs.sr.read().eop().bit_is_set() {
                self.regs.sr.modify(|_, w| w.eop().set_bit()); // Clear
            }
        }

        // 7. Clear the PG bit in the FLASH_CR register if there no more programming request
        // anymore.
        self.regs.cr.modify(|_, w| w.pg().clear_bit());

        self.lock();

        Ok(())
    }

    #[cfg(feature = "l5")]
    /// Write the contents of a page. Must be erased first. See L5 RM, section 6.3.7.
    pub fn write_page(
        &mut self,
        page: usize,
        data: &[u64],
        security: Security,
    ) -> Result<(), Error> {
        // todo: Consider a u8-based approach.
        // todo: DRY from `erase_page`.
        // The Flash memory programming sequence in standard mode is as follows:
        // 1. Check that no Flash main memory operation is ongoing by checking the NBBSY bit in the
        // Flash status register (FLASH_SR).
        self.unlock(security)?;

        match security {
            Security::NonSecure => {
                let sr = self.regs.nssr.read();
                if sr.nsbsy().bit_is_set() {
                    self.lock(security);
                    return Err(Error::Busy);
                }

                // 2. Check and clear all error programming flags due to a previous programming. If not,
                // NSPGSERR is set.
                if check_illegal(&self.regs, security).is_err() {
                    self.lock(security);
                    return Err(Error::Illegal);
                };

                // 3. Set the NSPG bit in tFLASH_NSCR register
                self.regs.nscr.modify(|_, w| w.nspg().set_bit());

                // todo: You have 3x DRY here re teh writing. Put that in  a fn?
                // 4. Perform the data write operation at the desired memory address, inside main memory
                // block or OTP area. Only double word can be programmed.
                let mut address = page_to_address(page) as *mut u32;

                for dword in data {
                    unsafe {
                        // – Write a first word in an address aligned with double word
                        core::ptr::write_volatile(address, *dword as u32);
                        // – Write the second word
                        core::ptr::write_volatile(address.add(1), (*dword >> 32) as u32);

                        address = address.add(2);
                    }

                    // 5. Wait until the BSY bit is cleared in the FLASH_NSSR register.
                    while self.regs.nssr.read().nsbsy().bit_is_set() {}

                    // 6. Check that NSEOP flag is set in the FLASH_NSSR register (meaning that the programming
                    // operation has succeed), and clear it by software.
                    if self.regs.nssr.read().nseop().bit_is_set() {
                        self.regs.nssr.modify(|_, w| w.nseop().set_bit());
                    } // todo: Else return error?
                }

                // 7. Clear the NSPG bit in the FLASH_CR register if there no more programming request
                // anymore.
                self.regs.nscr.modify(|_, w| w.nspg().clear_bit());
            }
            Security::Secure => {
                // Process here is monstly the same, but sub in sec registers and fields.
                let sr = self.regs.secsr.read();
                if sr.secbsy().bit_is_set() {
                    self.lock(security);
                    return Err(Error::Busy);
                }

                if check_illegal(&self.regs, security).is_err() {
                    self.lock(security);
                    return Err(Error::Illegal);
                };

                self.regs.seccr.modify(|_, w| w.secpg().set_bit());

                let mut address = page_to_address(page) as *mut u32;

                for dword in data {
                    unsafe {
                        // – Write a first word in an address aligned with double word
                        core::ptr::write_volatile(address, *dword as u32);
                        // – Write the second word
                        core::ptr::write_volatile(address.add(1), (*dword >> 32) as u32);

                        address = address.add(2);
                    }

                    while self.regs.secsr.read().secbsy().bit_is_set() {}

                    if self.regs.secsr.read().seceop().bit_is_set() {
                        self.regs.secsr.modify(|_, w| w.seceop().set_bit()); // clear
                    } // todo: Else return error?
                }

                self.regs.seccr.modify(|_, w| w.secpg().clear_bit());
            }
        }

        self.lock(security);

        Ok(())
    }

    /// Read a single 64-bit memory cell, indexed by its page, and an offset from the page.
    pub fn read(&self, page: usize, offset: isize) -> u64 {
        let addr = page_to_address(page) as *const u64;
        unsafe { core::ptr::read(addr.offset(offset)) }
    }

    /// Read flash memory at a given page and offset into a buffer.
    pub fn read_to_buffer(&self, page: usize, offset: isize, buff: &mut [u8]) {
        // todo: This is untested.
        let addr = page_to_address(page) as *const u8; // todo is this right?

        for val in buff {
            *val = unsafe { core::ptr::read(addr.offset(offset)) }
        }
    }
}

/// Calculate the address of the start of a given page. Each page is 2,048 Kb.
fn page_to_address(page: usize) -> usize {
    0x0800_0000 + page as usize * 2048
}
