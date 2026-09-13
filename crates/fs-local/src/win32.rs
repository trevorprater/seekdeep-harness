//! Safe re-exports of the narrow Win32 file-publication adapter.

pub use seekdeep_fs_local_win32_native::{
    copy_file_dacl as copy_file_dacl_win32, read_file_dacl as read_file_dacl_win32,
    replace_file as replace_file_win32,
};
