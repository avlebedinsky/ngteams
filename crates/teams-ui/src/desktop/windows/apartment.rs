/// Balanced `WinRT` initialization, always on a dedicated native worker thread.
pub(super) struct Apartment;
impl Apartment {
    pub(super) fn new() -> windows::core::Result<Self> {
        // SAFETY: a fresh app-owned thread; no other apartment is initialized there.
        unsafe {
            windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED)?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: balances the successful initialization on this same thread, after its COM objects drop.
        unsafe {
            windows::Win32::System::WinRT::RoUninitialize();
        }
    }
}
