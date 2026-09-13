//! Raw COM and user32 calls, isolated from the safe workspace.

use std::{ffi::c_void, ptr};

use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree, CoUninitialize,
            },
            LibraryLoader::{GetModuleHandleW, GetProcAddress},
            Threading::GetCurrentThreadId,
        },
        UI::{
            Shell::{FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_PICKFOLDERS, FileOpenDialog},
            WindowsAndMessaging::{EnumThreadWindows, PostMessageW, WM_CLOSE},
        },
    },
    core::{BOOL, GUID, HRESULT, PCWSTR, PWSTR},
};

const IID_IFILE_OPEN_DIALOG: GUID = GUID::from_u128(0xd57c7288_d4ad_4768_be02_9d969532d960);
const SIGDN_FILESYSPATH: i32 = 0x8005_8000_u32.cast_signed();
const HRESULT_CANCELLED: i32 = 0x8007_04c7_u32.cast_signed();

const SLOT_RELEASE: usize = 2;
const SLOT_SHOW: usize = 3;
const SLOT_SET_OPTIONS: usize = 9;
const SLOT_SET_TITLE: usize = 17;
const SLOT_GET_RESULT: usize = 20;
const SLOT_GET_DISPLAY_NAME: usize = 5;

type Release = unsafe extern "system" fn(*mut c_void) -> u32;
type Show = unsafe extern "system" fn(*mut c_void, HWND) -> HRESULT;
type SetOptions = unsafe extern "system" fn(*mut c_void, u32) -> HRESULT;
type SetTitle = unsafe extern "system" fn(*mut c_void, PCWSTR) -> HRESULT;
type GetResult = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT;
type GetDisplayName = unsafe extern "system" fn(*mut c_void, i32, *mut PWSTR) -> HRESULT;
type SetThreadDpiAwarenessContext = unsafe extern "system" fn(*mut c_void) -> *mut c_void;

struct ComObject(*mut c_void);

impl ComObject {
    fn new(pointer: *mut c_void, subject: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(!pointer.is_null(), "{subject} returned a null COM pointer");
        Ok(Self(pointer))
    }

    unsafe fn slot(&self, index: usize) -> *const c_void {
        // SAFETY: successful COM creation/result extraction guarantees the
        // interface pointer's first word is its immutable vtable pointer.
        let vtable = unsafe { *self.0.cast::<*const *const c_void>() };
        // SAFETY: slot constants are frozen IUnknown/IModalWindow/IFileDialog
        // and IShellItem ABI indices, available since Vista.
        unsafe { *vtable.add(index) }
    }
}

impl Drop for ComObject {
    fn drop(&mut self) {
        // SAFETY: this object owns one COM reference and slot 2 is IUnknown::Release.
        unsafe {
            let release: Release = std::mem::transmute(self.slot(SLOT_RELEASE));
            release(self.0);
        }
    }
}

struct TaskMem(PWSTR);

impl Drop for TaskMem {
    fn drop(&mut self) {
        // SAFETY: GetDisplayName allocates this string with the COM task allocator.
        unsafe { CoTaskMemFree(self.0.cast()) };
    }
}

struct ComApartment;

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: constructed only after a successful CoInitializeEx, including S_FALSE.
        unsafe { CoUninitialize() };
    }
}

pub(super) fn run_folder_dialog(
    title: &str,
    on_showing: impl FnOnce(u32),
) -> anyhow::Result<Option<String>> {
    set_thread_dpi_awareness();
    // SAFETY: null reserved pointer and the documented apartment flag.
    let initialized =
        unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED.cast_unsigned()) };
    check(initialized, "CoInitializeEx")?;
    let _apartment = ComApartment;

    let mut dialog_pointer = ptr::null_mut();
    // SAFETY: all GUIDs and the out pointer follow CoCreateInstance's ABI.
    let created = unsafe {
        CoCreateInstance(
            &FileOpenDialog,
            ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_IFILE_OPEN_DIALOG,
            &raw mut dialog_pointer,
        )
    };
    check(created, "CoCreateInstance(FileOpenDialog)")?;
    let dialog = ComObject::new(dialog_pointer, "CoCreateInstance(FileOpenDialog)")?;

    // SAFETY: each function pointer comes from the dialog's documented vtable
    // slot and is called with that same interface pointer.
    unsafe {
        let set_options: SetOptions = std::mem::transmute(dialog.slot(SLOT_SET_OPTIONS));
        check(
            set_options(
                dialog.0,
                FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR,
            ),
            "SetOptions",
        )?;
        let title = title.encode_utf16().chain([0]).collect::<Vec<_>>();
        let set_title: SetTitle = std::mem::transmute(dialog.slot(SLOT_SET_TITLE));
        check(set_title(dialog.0, title.as_ptr()), "SetTitle")?;
        on_showing(GetCurrentThreadId());
        let show: Show = std::mem::transmute(dialog.slot(SLOT_SHOW));
        let shown = show(dialog.0, ptr::null_mut());
        if shown == HRESULT_CANCELLED {
            return Ok(None);
        }
        check(shown, "Show")?;

        let mut item_pointer = ptr::null_mut();
        let get_result: GetResult = std::mem::transmute(dialog.slot(SLOT_GET_RESULT));
        check(get_result(dialog.0, &raw mut item_pointer), "GetResult")?;
        let item = ComObject::new(item_pointer, "GetResult")?;
        let mut name_pointer: PWSTR = ptr::null_mut();
        let get_display_name: GetDisplayName =
            std::mem::transmute(item.slot(SLOT_GET_DISPLAY_NAME));
        check(
            get_display_name(item.0, SIGDN_FILESYSPATH, &raw mut name_pointer),
            "GetResult",
        )?;
        anyhow::ensure!(
            !name_pointer.is_null(),
            "GetResult returned a null filesystem path"
        );
        let name = TaskMem(name_pointer);
        Ok(Some(read_utf16(name.0)))
    }
}

pub(super) fn close_thread_windows(thread_id: u32) {
    unsafe extern "system" fn close(hwnd: HWND, _parameter: LPARAM) -> BOOL {
        // SAFETY: hwnd comes from EnumThreadWindows and the scalar message
        // parameters carry no borrowed memory.
        unsafe {
            PostMessageW(hwnd, WM_CLOSE, WPARAM::default(), LPARAM::default());
        }
        1
    }
    // SAFETY: callback has the required system ABI and no captured state.
    unsafe {
        EnumThreadWindows(thread_id, Some(close), LPARAM::default());
    }
}

fn set_thread_dpi_awareness() {
    let user32 = "user32.dll\0".encode_utf16().collect::<Vec<_>>();
    // SAFETY: user32 is loaded in every GUI-capable Windows process and the
    // string is NUL-terminated.
    let module = unsafe { GetModuleHandleW(user32.as_ptr()) };
    if module.is_null() {
        return;
    }
    // SAFETY: the symbol name is NUL-terminated; absence is expected on old Windows.
    let symbol = unsafe { GetProcAddress(module, c"SetThreadDpiAwarenessContext".as_ptr().cast()) };
    let Some(symbol) = symbol else { return };
    // SAFETY: the resolved export has the documented user32 system ABI.
    let set_context: SetThreadDpiAwarenessContext = unsafe { std::mem::transmute(symbol) };
    for context in [-4_isize, -3, -2] {
        // SAFETY: these negative pseudo-handles are the documented DPI contexts.
        if !unsafe { set_context(context as *mut c_void) }.is_null() {
            return;
        }
    }
}

fn check(hr: i32, what: &str) -> anyhow::Result<i32> {
    if hr < 0 {
        anyhow::bail!("{what} failed: HRESULT 0x{:x}", hr.cast_unsigned())
    }
    Ok(hr)
}

unsafe fn read_utf16(pointer: PWSTR) -> String {
    let mut units = Vec::new();
    for index in 0..16_384 {
        // SAFETY: pointer is a NUL-terminated COM task string. The cap matches
        // the source adapter's 32 KiB view and bounds damage from a bad host.
        let unit = unsafe { *pointer.add(index) };
        if unit == 0 {
            break;
        }
        units.push(unit);
    }
    String::from_utf16_lossy(&units)
}
