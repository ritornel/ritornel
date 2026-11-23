//! The machine's name, as it was set: `Studio-PC`, where `COMPUTERNAME`
//! gives the NetBIOS name, which Windows upper-cases and cuts to 15
//! characters.

use std::io;

use windows_sys::Win32::Foundation::ERROR_MORE_DATA;
use windows_sys::Win32::System::SystemInformation::{
    ComputerNamePhysicalDnsHostname, GetComputerNameExW,
};

/// The machine's DNS host name, in the case it was set, without its domain:
/// the name it answers to by multicast DNS, and the one a person gave it.
pub fn dns_host_name() -> io::Result<String> {
    let mut length = 0_u32;
    // SAFETY: with no buffer and a length of 0, the call only writes the
    // length it needs, NUL included, through the pointer, which is to a
    // local that outlives it. It fails for want of room, as asked.
    let sized = unsafe {
        GetComputerNameExW(
            ComputerNamePhysicalDnsHostname,
            std::ptr::null_mut(),
            &raw mut length,
        )
    };
    // It must fail, and for want of the room it was asked into: any other
    // answer is the answer.
    let asked = io::Error::last_os_error();
    if sized != 0 || asked.raw_os_error() != i32::try_from(ERROR_MORE_DATA).ok() || length == 0 {
        return Err(asked);
    }
    let size = usize::try_from(length).map_err(io::Error::other)?;
    let mut name = vec![0_u16; size];
    // SAFETY: the buffer holds `length` UTF-16 units, as the call is told;
    // it writes at most that many and sets `length` to the units written,
    // the NUL left out.
    let done = unsafe {
        GetComputerNameExW(
            ComputerNamePhysicalDnsHostname,
            name.as_mut_ptr(),
            &raw mut length,
        )
    };
    if done == 0 {
        return Err(io::Error::last_os_error());
    }
    name.truncate(usize::try_from(length).map_err(io::Error::other)?);
    String::from_utf16(&name).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_machine_has_a_host_name() {
        let name = dns_host_name().unwrap();
        assert!(!name.is_empty());
        assert!(!name.contains('\0'));
        assert!(!name.contains('.'), "{name}");
    }
}
