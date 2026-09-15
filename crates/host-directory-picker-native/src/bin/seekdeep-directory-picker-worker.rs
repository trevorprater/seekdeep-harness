//! Blocking Win32 folder dialog child process.

use std::io::Write as _;

use seekdeep_host_directory_picker_native::win32_dialog::Win32DialogWorkerMessage;

fn post(message: &Win32DialogWorkerMessage) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, message)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let title = std::env::var("SEEKDEEP_DIALOG_TITLE")
        .map_err(|_| anyhow::anyhow!("win32-dialog-worker: SEEKDEEP_DIALOG_TITLE is required"))?;
    anyhow::ensure!(
        !title.is_empty(),
        "win32-dialog-worker: SEEKDEEP_DIALOG_TITLE is required"
    );
    let outcome = seekdeep_win32_directory_dialog::run_folder_dialog(&title, |thread_id| {
        let _ = post(&Win32DialogWorkerMessage::Showing { thread_id });
    });
    match outcome {
        Ok(path) => post(&Win32DialogWorkerMessage::Done { path }),
        Err(error) => post(&Win32DialogWorkerMessage::Error {
            message: format!("{error:#}"),
        }),
    }
}
