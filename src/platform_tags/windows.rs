use crate::prelude::*;

// https://docs.microsoft.com/en-us/windows/win32/sysinfo/image-file-machine-constants
const IMAGE_FILE_MACHINE_I386: u16 = 0x014c;
const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;

#[link(name = "kernel32")]
#[allow(non_snake_case)]
extern "system" {
    // This tells us which *non-native* platforms the system can run (e.g., x86-32 when
    // the system is x86-64, or x86-64 if the system is arm64). But it doesn't tell us
    // what the native platform is. For that we need...
    #[must_use]
    fn IsWow64GuestMachineSupported(machine: u16, out: *mut u8) -> u32;

    // ...this function. Together, we can get the full list of supported platforms.
    // (This is actually somewhat overkill right now since python packaging hasn't
    // defined a platform tag yet for ARM Windows wheels. But it's coming soon.)
    #[must_use]
    fn IsWowProcess2(
        hProcess: *const std::ffi::c_void,
        process_type: *mut u16,
        system_type: *mut u16,
    ) -> u32;
}

fn is_wow64_guest_machine_supported(machine: u16) -> Result<bool> {
    let mut out: u8 = 0;
    let hresult =
        unsafe { IsWow64GuestMachineSupported(machine, (&mut out) as *mut u8) };
    if hresult != 0 {
        Err(std::io::Error::last_os_error())?
    } else {
        Ok(out != 0)
    }
}

fn system_type() -> Result<u16> {
    let mut process_type: u16 = 0;
    let mut system_type: u16 = 0;
    let result = unsafe {
        IsWowProcess2(
            // the magic handle -1 means "the current process"
            -1 as *const std::ffi::c_void,
            &mut process_type as *mut u16,
            &mut system_type as *mut u16,
        )
    };
    if result != 0 {
        Err(std::io::Error::last_os_error())?
    } else {
        Ok((process_type, system_type))
    }
}

pub fn platform_tags() -> Result<Vec<String>> {
    let mut tags: Vec<String> = vec![];
    if cfg!(target_arch = "x86_64")
        || is_wow64_guest_machine_supported(IMAGE_FILE_MACHINE_AMD64)?
        || system_type()? == IMAGE_FILE_MACHINE_AMD64
    {
        tags.push("win_amd64".into());
    }
    if cfg!(target_arch = "x86")
        || is_wow64_guest_machine_supported(IMAGE_FILE_MACHINE_I386)?
        || system_type()? == IMAGE_FILE_MACHINE_I386
    {
        tags.push("win32".into());
    }
    Ok(tags)
}
